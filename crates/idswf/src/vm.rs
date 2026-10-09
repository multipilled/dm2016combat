//! ActionScript 2 interpreter covering the opcodes DOOM's GUIs use, plus native members of sprites,
//! edit texts and arrays.

use std::sync::Arc;

use anyhow::{Result, bail};

use crate::as2::{self, Action, PushValue};
use crate::bswf::{DictEntry, Matrix};
use crate::player::{AsFunction, Easing, Kind, Native, ObjId, Player, Value};

const MAX_STEPS: usize = 2_000_000;
const MAX_DEPTH: usize = 64;

struct Frame {
    code: Arc<[u8]>,
    end: usize,
    pool: Arc<Vec<Arc<str>>>,
    regs: Vec<Value>,
    stack: Vec<Value>,
    /// Scope chain, innermost last (activation object for functions).
    scope: Vec<ObjId>,
    this: Value,
    target: ObjId,
    activation: Option<ObjId>,
}

impl Player {
    pub(crate) fn run_code(&mut self, target: ObjId, code: Arc<[u8]>) -> Result<()> {
        let end = code.len();
        let mut f = Frame {
            code,
            end,
            pool: Arc::new(Vec::new()),
            regs: vec![Value::Undefined; 4],
            stack: Vec::new(),
            scope: vec![self.global, target],
            this: Value::Obj(target),
            target,
            activation: None,
        };
        self.exec(&mut f, 0, 0)?;
        Ok(())
    }

    pub fn call_value(&mut self, f: Value, this: Value, args: Vec<Value>) -> Result<Value> {
        self.call_depth(f, this, args, 0)
    }

    fn call_depth(&mut self, f: Value, this: Value, args: Vec<Value>, depth: usize) -> Result<Value> {
        let Some(fid) = f.as_obj() else { return Ok(Value::Undefined) };
        if depth > MAX_DEPTH {
            bail!("script recursion too deep");
        }
        let kind = match &self.obj(fid).kind {
            Kind::Function(func) => Ok(func.clone()),
            Kind::Native(n) => Err(*n),
            _ => return Ok(Value::Undefined),
        };
        match kind {
            Err(native) => self.call_native(native, this, args),
            Ok(func) => {
                let act = self.new_object();
                let mut regs = vec![Value::Undefined; (func.register_count as usize).max(4)];
                if func.v2 {
                    let mut r = 1usize;
                    let flags = func.flags;
                    let root = Value::Obj(self.root);
                    let parent = this.as_obj().and_then(|t| self.parent_of(t)).map(Value::Obj).unwrap_or_default();
                    let args_arr = Value::Obj(self.new_array(args.clone()));
                    let global = Value::Obj(self.global);
                    let mut put = |regs: &mut Vec<Value>, v: Value| {
                        if r < regs.len() {
                            regs[r] = v;
                        }
                        r += 1;
                    };
                    if flags & 0x01 != 0 {
                        put(&mut regs, this.clone());
                    }
                    if flags & 0x04 != 0 {
                        put(&mut regs, args_arr.clone());
                    }
                    if flags & 0x10 != 0 {
                        put(&mut regs, Value::Undefined);
                    }
                    if flags & 0x40 != 0 {
                        put(&mut regs, root);
                    }
                    if flags & 0x80 != 0 {
                        put(&mut regs, parent);
                    }
                    if flags & 0x100 != 0 {
                        put(&mut regs, global);
                    }
                    if flags & 0x08 == 0 {
                        self.set_prop(act, "arguments", args_arr);
                    }
                    if flags & 0x02 == 0 {
                        self.set_prop(act, "this", this.clone());
                    }
                } else {
                    let a = Value::Obj(self.new_array(args.clone()));
                    self.set_prop(act, "arguments", a);
                    self.set_prop(act, "this", this.clone());
                }
                for (i, (reg, name)) in func.params.iter().enumerate() {
                    let v = args.get(i).cloned().unwrap_or_default();
                    if func.v2 && *reg != 0 {
                        if (*reg as usize) < regs.len() {
                            regs[*reg as usize] = v;
                        }
                    } else {
                        self.set_prop(act, name, v);
                    }
                }
                let mut scope = func.scope.clone();
                scope.push(act);
                let mut f = Frame {
                    code: func.code.clone(),
                    end: func.body.end.min(func.code.len()),
                    pool: func.pool.clone(),
                    regs,
                    stack: Vec::new(),
                    scope,
                    this,
                    target: func.target,
                    activation: Some(act),
                };
                self.exec(&mut f, func.body.start, depth + 1)
            }
        }
    }

    fn exec(&mut self, f: &mut Frame, mut pc: usize, depth: usize) -> Result<Value> {
        let mut steps = 0;
        let mut with_stack: Vec<(ObjId, usize)> = Vec::new();
        while pc < f.end {
            steps += 1;
            if steps > MAX_STEPS {
                bail!("script step limit reached");
            }
            while let Some(&(_, end)) = with_stack.last() {
                if pc >= end {
                    with_stack.pop();
                    f.scope.pop();
                } else {
                    break;
                }
            }
            let (a, next) = as2::decode(&f.code, pc)?;
            pc = next;
            match a {
                Action::Simple(op) => {
                    if let Some(ret) = self.simple(f, op, depth)? {
                        return Ok(ret);
                    }
                    if op == 0 {
                        return Ok(Value::Undefined);
                    }
                }
                Action::Push(vals) => {
                    for v in vals {
                        let v = match v {
                            PushValue::Str(s) => Value::str(&s),
                            PushValue::Float(x) => Value::Num(x as f64),
                            PushValue::Null => Value::Null,
                            PushValue::Undefined => Value::Undefined,
                            PushValue::Register(r) => f.regs.get(r as usize).cloned().unwrap_or_default(),
                            PushValue::Bool(b) => Value::Bool(b),
                            PushValue::Double(x) => Value::Num(x),
                            PushValue::Int(i) => Value::Num(i as f64),
                            PushValue::Constant(i) => f.pool.get(i as usize).map(|s| Value::Str(s.clone())).unwrap_or_default(),
                        };
                        f.stack.push(v);
                    }
                }
                Action::ConstantPool(v) => f.pool = Arc::new(v.iter().map(|s| Arc::from(s.as_str())).collect()),
                Action::StoreRegister(r) => {
                    let v = f.stack.last().cloned().unwrap_or_default();
                    if (r as usize) < f.regs.len() {
                        f.regs[r as usize] = v;
                    } else {
                        f.regs.resize(r as usize + 1, Value::Undefined);
                        f.regs[r as usize] = v;
                    }
                }
                Action::Jump(o) => pc = (pc as i64 + o as i64) as usize,
                Action::If(o) => {
                    let c = f.stack.pop().unwrap_or_default();
                    if self.to_bool(&c) {
                        pc = (pc as i64 + o as i64) as usize;
                    }
                }
                Action::DefineFunction(func) => {
                    let body = func.body.clone();
                    let af = AsFunction {
                        code: f.code.clone(),
                        body: body.clone(),
                        params: func.params.iter().map(|p| (p.register, Arc::from(p.name.as_str()))).collect(),
                        register_count: func.register_count,
                        flags: func.flags,
                        v2: func.v2,
                        pool: f.pool.clone(),
                        scope: f.scope.clone(),
                        target: f.target,
                    };
                    let proto = self.object_proto;
                    let fid = self.alloc(Kind::Function(Arc::new(af)), Some(proto));
                    let p = self.new_object();
                    self.set_prop(fid, "prototype", Value::Obj(p));
                    if func.name.is_empty() {
                        f.stack.push(Value::Obj(fid));
                    } else {
                        let holder = f.activation.unwrap_or(f.target);
                        self.set_prop(holder, &func.name, Value::Obj(fid));
                    }
                    pc = body.end;
                }
                Action::GotoFrame(n) => {
                    let t = f.target;
                    self.goto(t, Value::Num(n as f64 + 1.0), false);
                }
                Action::GotoLabel(l) => {
                    let t = f.target;
                    self.goto(t, Value::str(&l), false);
                }
                Action::GotoFrame2 { play, bias } => {
                    let v = f.stack.pop().unwrap_or_default();
                    let v = match (v, bias) {
                        (Value::Num(n), Some(b)) => Value::Num(n + b as f64),
                        (v, _) => v,
                    };
                    let t = f.target;
                    self.goto(t, v, play);
                }
                Action::SetTarget(name) => {
                    if name.is_empty() {
                        f.target = f.scope.get(1).copied().unwrap_or(self.root);
                    } else if let Some(t) = self.resolve_target(f.target, &name) {
                        f.target = t;
                    }
                }
                Action::With(size) => {
                    let o = f.stack.pop().unwrap_or_default();
                    if let Some(id) = o.as_obj() {
                        f.scope.push(id);
                        with_stack.push((id, pc + size as usize));
                    } else {
                        pc += size as usize;
                    }
                }
                Action::GetUrl(..) | Action::GetUrl2(_) | Action::WaitForFrame(..) | Action::WaitForFrame2(_) | Action::Call => {}
                Action::Try { .. } => bail!("try/catch not supported"),
                Action::Unknown(op, _) => bail!("unknown action {op:#x}"),
            }
        }
        Ok(Value::Undefined)
    }

    /// Returns Some(value) for `return`.
    fn simple(&mut self, f: &mut Frame, op: u8, depth: usize) -> Result<Option<Value>> {
        macro_rules! pop {
            () => {
                f.stack.pop().unwrap_or_default()
            };
        }
        match op {
            0x00 => {}
            0x04 => {
                let t = f.target;
                let fr = self.sprite(t).map(|s| s.frame + 1).unwrap_or(1);
                self.goto(t, Value::Num(fr as f64), false);
            }
            0x05 => {
                let t = f.target;
                let fr = self.sprite(t).map(|s| s.frame.saturating_sub(1).max(1)).unwrap_or(1);
                self.goto(t, Value::Num(fr as f64), false);
            }
            0x06 => {
                if let Some(s) = self.sprite_mut(f.target) {
                    s.playing = true;
                }
            }
            0x07 => {
                if let Some(s) = self.sprite_mut(f.target) {
                    s.playing = false;
                }
            }
            0x08 | 0x09 => {}
            0x0a | 0x0b | 0x0c | 0x0d | 0x0f | 0x0e => {
                let b = pop!();
                let a = pop!();
                let (x, y) = (self.to_number(&a), self.to_number(&b));
                f.stack.push(match op {
                    0x0a => Value::Num(x + y),
                    0x0b => Value::Num(x - y),
                    0x0c => Value::Num(x * y),
                    0x0d => Value::Num(x / y),
                    0x0e => Value::Bool(x == y),
                    _ => Value::Bool(x < y),
                });
            }
            0x10 | 0x11 => {
                let b = pop!();
                let a = pop!();
                let (x, y) = (self.to_bool(&a), self.to_bool(&b));
                f.stack.push(Value::Bool(if op == 0x10 { x && y } else { x || y }));
            }
            0x12 => {
                let a = pop!();
                let b = self.to_bool(&a);
                f.stack.push(Value::Bool(!b));
            }
            0x13 => {
                let b = pop!();
                let a = pop!();
                f.stack.push(Value::Bool(self.to_string(&a) == self.to_string(&b)));
            }
            0x14 | 0x31 => {
                let a = pop!();
                f.stack.push(Value::Num(self.to_string(&a).chars().count() as f64));
            }
            0x15 | 0x35 => {
                let count = pop!();
                let index = pop!();
                let s = pop!();
                let s = self.to_string(&s);
                let i = (self.to_number(&index) as i64 - 1).max(0) as usize;
                let n = self.to_number(&count).max(0.0) as usize;
                f.stack.push(Value::str(&s.chars().skip(i).take(n).collect::<String>()));
            }
            0x17 => {
                f.stack.pop();
            }
            0x18 => {
                let a = pop!();
                f.stack.push(Value::Num(self.to_number(&a).trunc()));
            }
            0x1c => {
                let name = pop!();
                let name = self.to_string(&name);
                let v = self.get_variable(f, &name);
                f.stack.push(v);
            }
            0x1d => {
                let v = pop!();
                let name = pop!();
                let name = self.to_string(&name);
                self.set_variable(f, &name, v);
            }
            0x20 => {
                let t = pop!();
                match t {
                    Value::Obj(id) => f.target = id,
                    other => {
                        let s = self.to_string(&other);
                        if s.is_empty() {
                            f.target = f.scope.get(1).copied().unwrap_or(self.root);
                        } else if let Some(id) = self.resolve_target(f.target, &s) {
                            f.target = id;
                        }
                    }
                }
            }
            0x21 => {
                let b = pop!();
                let a = pop!();
                f.stack.push(Value::str(&(self.to_string(&a) + &self.to_string(&b))));
            }
            0x22 => {
                let idx = pop!();
                let tgt = pop!();
                let obj = self.target_value(f, &tgt);
                let name = property_name(self.to_number(&idx) as i32);
                let v = match obj {
                    Some(o) => self.get_member(Value::Obj(o), name),
                    None => Value::Undefined,
                };
                f.stack.push(v);
            }
            0x23 => {
                let v = pop!();
                let idx = pop!();
                let tgt = pop!();
                if let Some(o) = self.target_value(f, &tgt) {
                    let name = property_name(self.to_number(&idx) as i32);
                    self.set_member(Value::Obj(o), name, v);
                }
            }
            0x24 | 0x25 | 0x27 | 0x28 => {
                let n = match op {
                    0x24 => 3,
                    0x25 => 1,
                    0x27 => 3,
                    _ => 0,
                };
                for _ in 0..n {
                    f.stack.pop();
                }
            }
            0x26 => {
                let a = pop!();
                let s = self.to_string(&a);
                self.note(format!("trace: {s}"));
            }
            0x29 => {
                let b = pop!();
                let a = pop!();
                f.stack.push(Value::Bool(self.to_string(&a) < self.to_string(&b)));
            }
            0x68 => {
                let b = pop!();
                let a = pop!();
                f.stack.push(Value::Bool(self.to_string(&a) > self.to_string(&b)));
            }
            0x30 => {
                let n = pop!();
                let n = self.to_number(&n).max(0.0);
                let r = (self.random() * n).floor();
                f.stack.push(Value::Num(r));
            }
            0x32 | 0x36 => {
                let a = pop!();
                let s = self.to_string(&a);
                f.stack.push(Value::Num(s.chars().next().map(|c| c as u32 as f64).unwrap_or(0.0)));
            }
            0x33 | 0x37 => {
                let a = pop!();
                let c = char::from_u32(self.to_number(&a) as u32).unwrap_or('\0');
                f.stack.push(Value::str(&c.to_string()));
            }
            0x34 => f.stack.push(Value::Num((self.time * 1000.0).floor())),
            0x3a => {
                let name = pop!();
                let obj = pop!();
                let name = self.to_string(&name);
                let ok = match obj.as_obj() {
                    Some(o) => {
                        let props = &mut self.obj_mut(o).props;
                        let before = props.len();
                        props.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
                        props.len() != before
                    }
                    None => false,
                };
                f.stack.push(Value::Bool(ok));
            }
            0x3b => {
                let name = pop!();
                let name = self.to_string(&name);
                let mut ok = false;
                for &s in f.scope.iter().rev() {
                    let props = &mut self.obj_mut(s).props;
                    let before = props.len();
                    props.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
                    if props.len() != before {
                        ok = true;
                        break;
                    }
                }
                f.stack.push(Value::Bool(ok));
            }
            0x3c => {
                let v = pop!();
                let name = pop!();
                let name = self.to_string(&name);
                let holder = f.activation.unwrap_or(f.target);
                self.set_prop(holder, &name, v);
            }
            0x41 => {
                let name = pop!();
                let name = self.to_string(&name);
                let holder = f.activation.unwrap_or(f.target);
                if self.get_prop(holder, &name).is_none() {
                    self.set_prop(holder, &name, Value::Undefined);
                }
            }
            0x3d => {
                let name = pop!();
                let name = self.to_string(&name);
                let n = pop!();
                let n = self.to_number(&n).max(0.0) as usize;
                let args: Vec<Value> = (0..n).map(|_| f.stack.pop().unwrap_or_default()).collect();
                let (func, this) = self.lookup_function(f, &name);
                let r = if func.is_undefined() {
                    self.note(format!("call to undefined function '{name}'"));
                    Value::Undefined
                } else {
                    self.call_depth(func, this, args, depth)?
                };
                f.stack.push(r);
            }
            0x52 | 0x53 => {
                let name = pop!();
                let obj = pop!();
                let n = pop!();
                let n = self.to_number(&n).max(0.0) as usize;
                let args: Vec<Value> = (0..n).map(|_| f.stack.pop().unwrap_or_default()).collect();
                let method = if name.is_undefined() { String::new() } else { self.to_string(&name) };
                let r = if op == 0x52 {
                    if method.is_empty() {
                        self.call_depth(obj, Value::Undefined, args, depth)?
                    } else {
                        let func = self.get_member(obj.clone(), &method);
                        if func.is_undefined() {
                            self.note(format!("call to undefined method '{method}'"));
                            Value::Undefined
                        } else {
                            self.call_depth(func, obj, args, depth)?
                        }
                    }
                } else {
                    let ctor = if method.is_empty() { obj } else { self.get_member(obj, &method) };
                    self.construct(ctor, args, depth)?
                };
                f.stack.push(r);
            }
            0x40 => {
                let name = pop!();
                let name = self.to_string(&name);
                let n = pop!();
                let n = self.to_number(&n).max(0.0) as usize;
                let args: Vec<Value> = (0..n).map(|_| f.stack.pop().unwrap_or_default()).collect();
                let ctor = self.get_variable(f, &name);
                let r = self.construct(ctor, args, depth)?;
                f.stack.push(r);
            }
            0x3e => {
                return Ok(Some(f.stack.pop().unwrap_or_default()));
            }
            0x3f => {
                let b = pop!();
                let a = pop!();
                f.stack.push(Value::Num(self.to_number(&a) % self.to_number(&b)));
            }
            0x42 => {
                let n = pop!();
                let n = self.to_number(&n).max(0.0) as usize;
                let items: Vec<Value> = (0..n).map(|_| f.stack.pop().unwrap_or_default()).collect();
                let a = self.new_array(items);
                f.stack.push(Value::Obj(a));
            }
            0x43 => {
                let n = pop!();
                let n = self.to_number(&n).max(0.0) as usize;
                let o = self.new_object();
                for _ in 0..n {
                    let v = pop!();
                    let k = pop!();
                    let k = self.to_string(&k);
                    self.set_prop(o, &k, v);
                }
                f.stack.push(Value::Obj(o));
            }
            0x44 => {
                let a = pop!();
                let t = match &a {
                    Value::Undefined => "undefined",
                    Value::Null => "null",
                    Value::Bool(_) => "boolean",
                    Value::Num(_) => "number",
                    Value::Str(_) => "string",
                    Value::Obj(o) => match &self.obj(*o).kind {
                        Kind::Function(_) | Kind::Native(_) => "function",
                        Kind::Sprite(_) => "movieclip",
                        _ => "object",
                    },
                };
                f.stack.push(Value::str(t));
            }
            0x45 => {
                let a = pop!();
                let p = a.as_obj().map(|o| self.target_path(o)).unwrap_or_default();
                f.stack.push(Value::str(&p));
            }
            0x46 | 0x55 => {
                let o = pop!();
                let o = if op == 0x46 {
                    let n = self.to_string(&o);
                    self.get_variable(f, &n)
                } else {
                    o
                };
                f.stack.push(Value::Null);
                if let Some(id) = o.as_obj() {
                    let mut keys: Vec<Arc<str>> = self.obj(id).props.iter().map(|(k, _)| k.clone()).collect();
                    if let Kind::Array(items) = &self.obj(id).kind {
                        keys.extend((0..items.len()).map(|i| Arc::from(i.to_string().as_str())));
                    }
                    for k in keys {
                        f.stack.push(Value::Str(k));
                    }
                }
            }
            0x47 => {
                let b = pop!();
                let a = pop!();
                let (pa, pb) = (self.to_primitive(&a), self.to_primitive(&b));
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    f.stack.push(Value::str(&(self.to_string(&pa) + &self.to_string(&pb))));
                } else {
                    f.stack.push(Value::Num(self.to_number(&pa) + self.to_number(&pb)));
                }
            }
            0x48 | 0x67 => {
                let b = pop!();
                let a = pop!();
                let (x, y) = if op == 0x48 { (a, b) } else { (b, a) };
                let (px, py) = (self.to_primitive(&x), self.to_primitive(&y));
                let r = match (&px, &py) {
                    (Value::Str(s1), Value::Str(s2)) => Value::Bool(s1 < s2),
                    _ => {
                        let (n1, n2) = (self.to_number(&px), self.to_number(&py));
                        if n1.is_nan() || n2.is_nan() { Value::Undefined } else { Value::Bool(n1 < n2) }
                    }
                };
                f.stack.push(r);
            }
            0x49 => {
                let b = pop!();
                let a = pop!();
                let r = self.abstract_equals(&a, &b);
                f.stack.push(Value::Bool(r));
            }
            0x66 => {
                let b = pop!();
                let a = pop!();
                let r = strict_equals(&a, &b);
                f.stack.push(Value::Bool(r));
            }
            0x4a => {
                let a = pop!();
                f.stack.push(Value::Num(self.to_number(&a)));
            }
            0x4b => {
                let a = pop!();
                f.stack.push(Value::str(&self.to_string(&a)));
            }
            0x4c => {
                let a = f.stack.last().cloned().unwrap_or_default();
                f.stack.push(a);
            }
            0x4d => {
                let n = f.stack.len();
                if n >= 2 {
                    f.stack.swap(n - 1, n - 2);
                }
            }
            0x4e => {
                let name = pop!();
                let obj = pop!();
                let v = match &name {
                    Value::Num(n) => self.get_index(&obj, *n),
                    _ => {
                        let s = self.to_string(&name);
                        self.get_member(obj, &s)
                    }
                };
                f.stack.push(v);
            }
            0x4f => {
                let v = pop!();
                let name = pop!();
                let obj = pop!();
                match &name {
                    Value::Num(n) => self.set_index(&obj, *n, v),
                    _ => {
                        let s = self.to_string(&name);
                        self.set_member(obj, &s, v);
                    }
                }
            }
            0x50 | 0x51 => {
                let a = pop!();
                let n = self.to_number(&a);
                f.stack.push(Value::Num(if op == 0x50 { n + 1.0 } else { n - 1.0 }));
            }
            0x54 => {
                let ctor = pop!();
                let obj = pop!();
                let mut r = false;
                if let (Some(o), Some(c)) = (obj.as_obj(), ctor.as_obj()) {
                    let proto = self.get_prop(c, "prototype").and_then(Value::as_obj);
                    let mut p = self.obj(o).proto;
                    while let Some(pp) = p {
                        if Some(pp) == proto {
                            r = true;
                            break;
                        }
                        p = self.obj(pp).proto;
                    }
                }
                f.stack.push(Value::Bool(r));
            }
            0x60..=0x65 => {
                let b = pop!();
                let a = pop!();
                let x = to_i32(self.to_number(&a));
                let y = to_i32(self.to_number(&b));
                let r = match op {
                    0x60 => (x & y) as f64,
                    0x61 => (x | y) as f64,
                    0x62 => (x ^ y) as f64,
                    0x63 => (x << (y & 31)) as f64,
                    0x64 => (x >> (y & 31)) as f64,
                    _ => ((x as u32) >> (y & 31)) as f64,
                };
                f.stack.push(Value::Num(r));
            }
            0x2a => bail!("throw"),
            0x2b | 0x2c | 0x69 => {
                f.stack.pop();
            }
            _ => bail!("unsupported action {op:#x} ({})", as2::simple_name(op)),
        }
        Ok(None)
    }

    fn construct(&mut self, ctor: Value, args: Vec<Value>, depth: usize) -> Result<Value> {
        let Some(cid) = ctor.as_obj() else { return Ok(Value::Undefined) };
        match &self.obj(cid).kind {
            Kind::Native(Native::ArrayCtor) => {
                if args.len() == 1 {
                    if let Value::Num(n) = args[0] {
                        let a = self.new_array(vec![Value::Undefined; n.max(0.0) as usize]);
                        return Ok(Value::Obj(a));
                    }
                }
                Ok(Value::Obj(self.new_array(args)))
            }
            Kind::Native(Native::ObjectCtor) => Ok(Value::Obj(self.new_object())),
            Kind::Function(_) => {
                let proto = self.get_prop(cid, "prototype").and_then(Value::as_obj).unwrap_or(self.object_proto);
                let o = self.alloc(Kind::Plain, Some(proto));
                self.call_depth(ctor, Value::Obj(o), args, depth)?;
                Ok(Value::Obj(o))
            }
            _ => Ok(Value::Obj(self.new_object())),
        }
    }

    // ---------------------------------------------------------------- variables

    fn special(&self, f: &Frame, name: &str) -> Option<Value> {
        Some(match name {
            "this" => f.this.clone(),
            "_root" | "_level0" => Value::Obj(self.root),
            "_global" => Value::Obj(self.global),
            "_parent" => self.parent_of(f.target).map(Value::Obj).unwrap_or_default(),
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return None,
        })
    }

    fn get_variable(&mut self, f: &Frame, name: &str) -> Value {
        if name.contains(['.', '/', ':']) {
            return self.get_path(f, name);
        }
        if let Some(v) = self.special(f, name) {
            return v;
        }
        for &s in f.scope.iter().rev() {
            if let Some(v) = self.lookup_member(s, name) {
                return v;
            }
        }
        // Fall back to the current target (SetTarget may have changed it).
        self.lookup_member(f.target, name).unwrap_or_default()
    }

    fn get_path(&mut self, f: &Frame, path: &str) -> Value {
        let (obj_path, var) = match path.rsplit_once(':') {
            Some((o, v)) => (o, Some(v)),
            None => (path, None),
        };
        let mut parts = obj_path.split(['.', '/']).filter(|s| !s.is_empty());
        let mut cur = match parts.next() {
            Some(first) => {
                if let Some(v) = self.special(f, first) {
                    v
                } else {
                    let mut v = Value::Undefined;
                    for &s in f.scope.iter().rev() {
                        if let Some(x) = self.lookup_member(s, first) {
                            v = x;
                            break;
                        }
                    }
                    v
                }
            }
            None => Value::Obj(self.root),
        };
        for p in parts {
            cur = if p == ".." { cur.as_obj().and_then(|o| self.parent_of(o)).map(Value::Obj).unwrap_or_default() } else { self.get_member(cur, p) };
        }
        match var {
            Some(v) => self.get_member(cur, v),
            None => cur,
        }
    }

    fn set_variable(&mut self, f: &Frame, name: &str, v: Value) {
        if let Some((obj_path, var)) = name.rsplit_once(['.', ':']) {
            let o = self.get_path(f, obj_path);
            self.set_member(o, var, v);
            return;
        }
        for &s in f.scope.iter().rev().take(f.scope.len().saturating_sub(1)) {
            if Some(s) == f.activation && self.get_prop(s, name).is_some() {
                self.set_prop(s, name, v);
                return;
            }
        }
        self.set_member(Value::Obj(f.target), name, v);
    }

    fn lookup_function(&mut self, f: &Frame, name: &str) -> (Value, Value) {
        if name.contains(['.', '/', ':']) {
            let v = self.get_path(f, name);
            return (v, Value::Obj(f.target));
        }
        for &s in f.scope.iter().rev() {
            if let Some(v) = self.lookup_member(s, name) {
                let this = match &self.obj(s).kind {
                    Kind::Sprite(_) | Kind::Text(_) => Value::Obj(s),
                    _ => Value::Obj(f.target),
                };
                return (v, this);
            }
        }
        (Value::Undefined, Value::Undefined)
    }

    fn resolve_target(&mut self, from: ObjId, path: &str) -> Option<ObjId> {
        let mut cur = if path.starts_with('/') { self.root } else { from };
        for p in path.split(['.', '/']).filter(|s| !s.is_empty()) {
            cur = match p {
                "_root" | "_level0" => self.root,
                "_parent" | ".." => self.parent_of(cur)?,
                "this" => cur,
                _ => self.get_member(Value::Obj(cur), p).as_obj()?,
            };
        }
        Some(cur)
    }

    fn target_value(&mut self, f: &Frame, v: &Value) -> Option<ObjId> {
        match v {
            Value::Obj(o) => Some(*o),
            other => {
                let s = self.to_string(other);
                if s.is_empty() { Some(f.target) } else { self.resolve_target(f.target, &s) }
            }
        }
    }

    /// Own/native member or prototype member, None when absent.
    fn lookup_member(&mut self, id: ObjId, name: &str) -> Option<Value> {
        if let Some(v) = self.native_get(id, name) {
            return Some(v);
        }
        let mut cur = Some(id);
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(v) = self.get_prop(c, name) {
                return Some(v.clone());
            }
            cur = self.obj(c).proto;
            n += 1;
            if n > 32 {
                break;
            }
        }
        None
    }

    pub fn get_member(&mut self, obj: Value, name: &str) -> Value {
        match obj {
            Value::Obj(id) => self.lookup_member(id, name).unwrap_or_default(),
            Value::Str(s) => match name {
                "length" => Value::Num(s.chars().count() as f64),
                _ => Value::Undefined,
            },
            _ => Value::Undefined,
        }
    }

    fn get_index(&mut self, obj: &Value, n: f64) -> Value {
        if let Some(id) = obj.as_obj() {
            if let Kind::Array(items) = &self.obj(id).kind {
                return items.get(n as usize).cloned().unwrap_or_default();
            }
        }
        let s = num_to_string(n);
        self.get_member(obj.clone(), &s)
    }

    fn set_index(&mut self, obj: &Value, n: f64, v: Value) {
        if let Some(id) = obj.as_obj() {
            if let Kind::Array(items) = &mut self.obj_mut(id).kind {
                let i = n.max(0.0) as usize;
                if i >= items.len() {
                    items.resize(i + 1, Value::Undefined);
                }
                items[i] = v;
                return;
            }
        }
        let s = num_to_string(n);
        self.set_member(obj.clone(), &s, v);
    }

    pub fn set_member(&mut self, obj: Value, name: &str, v: Value) {
        let Some(id) = obj.as_obj() else { return };
        if self.native_set(id, name, &v) {
            return;
        }
        if let Kind::Array(items) = &mut self.obj_mut(id).kind {
            if let Ok(i) = name.parse::<usize>() {
                if i >= items.len() {
                    items.resize(i + 1, Value::Undefined);
                }
                items[i] = v;
                return;
            }
            if name == "length" {
                let n = match v {
                    Value::Num(n) => n.max(0.0) as usize,
                    _ => return,
                };
                items.resize(n, Value::Undefined);
                return;
            }
        }
        self.set_prop(id, name, v);
    }

    // ---------------------------------------------------------------- natives

    fn native_get(&mut self, id: ObjId, name: &str) -> Option<Value> {
        match &self.obj(id).kind {
            Kind::Array(items) => {
                if name == "length" {
                    return Some(Value::Num(items.len() as f64));
                }
                if let Ok(i) = name.parse::<usize>() {
                    return Some(items.get(i).cloned().unwrap_or_default());
                }
                None
            }
            Kind::Sprite(s) => {
                let d = self.display_of(id);
                let m = d.map(|d| d.matrix).unwrap_or(Matrix::IDENTITY);
                let v = match name {
                    "_x" => Value::Num(m.tx as f64),
                    "_y" => Value::Num(m.ty as f64),
                    "_xscale" => Value::Num(((m.xx * m.xx + m.yx * m.yx).sqrt() * 100.0) as f64),
                    "_yscale" => Value::Num(((m.yy * m.yy + m.xy * m.xy).sqrt() * 100.0) as f64),
                    "_rotation" => Value::Num((m.yx as f64).atan2(m.xx as f64).to_degrees()),
                    "_alpha" => Value::Num(d.map(|d| d.cxform.mul[3]).unwrap_or(1.0) as f64),
                    "_visible" => Value::Bool(d.map(|d| d.visible).unwrap_or(true)),
                    "_brightness" => Value::Num(s.brightness as f64),
                    "_name" => Value::Str(s.name.clone()),
                    "_currentframe" => Value::Num(s.frame as f64),
                    "_totalframes" | "_framesloaded" => Value::Num(self.frame_count(id) as f64),
                    "_target" => Value::str(&self.target_path(id)),
                    "_parent" => s.parent.map(Value::Obj).unwrap_or_default(),
                    "_root" => Value::Obj(self.root),
                    "_width" | "_height" => {
                        let b = self.bounds(id);
                        let (w, h) = (b.br[0] - b.tl[0], b.br[1] - b.tl[1]);
                        let sx = (m.xx * m.xx + m.yx * m.yx).sqrt();
                        let sy = (m.yy * m.yy + m.xy * m.xy).sqrt();
                        Value::Num(if name == "_width" { (w * sx) as f64 } else { (h * sy) as f64 })
                    }
                    "material" => s.material.clone().map(Value::Str).unwrap_or_default(),
                    "materialWidth" => Value::Num(s.material_width as f64),
                    "materialHeight" => Value::Num(s.material_height as f64),
                    "_z" | "xOffset" => Value::Num(0.0),
                    _ => return self.child(id, name).map(Value::Obj),
                };
                Some(v)
            }
            Kind::Text(t) => {
                let d = self.display_of(id);
                let v = match name {
                    "text" => Value::str(&t.text),
                    "textColor" => Value::Num(((t.color[0] as u32) << 16 | (t.color[1] as u32) << 8 | t.color[2] as u32) as f64),
                    "_alpha" => Value::Num(d.map(|d| d.cxform.mul[3]).unwrap_or(1.0) as f64),
                    "_visible" => Value::Bool(d.map(|d| d.visible).unwrap_or(true)),
                    "_x" => Value::Num(d.map(|d| d.matrix.tx).unwrap_or(0.0) as f64),
                    "_y" => Value::Num(d.map(|d| d.matrix.ty).unwrap_or(0.0) as f64),
                    "_name" => Value::Str(t.name.clone()),
                    "_parent" => Value::Obj(t.parent),
                    "variable" => Value::str(&t.variable),
                    "_textLength" | "length" => Value::Num(t.text.chars().count() as f64),
                    "fontFX" | "_fontFXMaterialIndex" => Value::Num(t.font_fx as f64),
                    "scroll" => Value::Num(0.0),
                    "maxscroll" => Value::Num(0.0),
                    _ => return None,
                };
                Some(v)
            }
            _ => None,
        }
    }

    fn native_set(&mut self, id: ObjId, name: &str, v: &Value) -> bool {
        let is_sprite = matches!(self.obj(id).kind, Kind::Sprite(_));
        let is_text = matches!(self.obj(id).kind, Kind::Text(_));
        if !is_sprite && !is_text {
            return false;
        }
        let num = self.to_number(v) as f32;
        let flag = self.to_bool(v);
        if is_text {
            match name {
                "text" => {
                    let s = self.to_string(v);
                    self.text_mut(id).unwrap().text = s;
                    return true;
                }
                "textColor" => {
                    let c = num as u32;
                    let t = self.text_mut(id).unwrap();
                    t.color[0] = (c >> 16) as u8;
                    t.color[1] = (c >> 8) as u8;
                    t.color[2] = c as u8;
                    return true;
                }
                "fontFX" | "_fontFXMaterialIndex" => {
                    self.text_mut(id).unwrap().font_fx = num as i32;
                    return true;
                }
                "variable" => {
                    let s = self.to_string(v);
                    self.text_mut(id).unwrap().variable = s;
                    return true;
                }
                "_alpha" | "_visible" | "_x" | "_y" => {}
                _ => return false,
            }
        }
        if is_sprite {
            match name {
                "material" => {
                    let s = if v.is_undefined() { None } else { Some(Arc::from(self.to_string(v).as_str())) };
                    self.sprite_mut(id).unwrap().material = s;
                    return true;
                }
                "materialWidth" => {
                    self.sprite_mut(id).unwrap().material_width = num as i32;
                    return true;
                }
                "materialHeight" => {
                    self.sprite_mut(id).unwrap().material_height = num as i32;
                    return true;
                }
                "_brightness" => {
                    self.sprite_mut(id).unwrap().brightness = num;
                    return true;
                }
                "onEnterFrame" | "_name" => {}
                _ => {}
            }
        }
        let Some(d) = self.display_of_mut(id) else { return false };
        let m = &mut d.matrix;
        match name {
            "_x" => m.tx = num,
            "_y" => m.ty = num,
            "_alpha" => d.cxform.mul[3] = num,
            "_visible" => d.visible = flag,
            "_xscale" => {
                let cur = (m.xx * m.xx + m.yx * m.yx).sqrt();
                let k = if cur > 1e-6 { num / 100.0 / cur } else { 0.0 };
                if cur > 1e-6 {
                    m.xx *= k;
                    m.yx *= k;
                } else {
                    m.xx = num / 100.0;
                    m.yx = 0.0;
                }
            }
            "_yscale" => {
                let cur = (m.yy * m.yy + m.xy * m.xy).sqrt();
                if cur > 1e-6 {
                    let k = num / 100.0 / cur;
                    m.yy *= k;
                    m.xy *= k;
                } else {
                    m.yy = num / 100.0;
                    m.xy = 0.0;
                }
            }
            "_rotation" => {
                let sx = (m.xx * m.xx + m.yx * m.yx).sqrt();
                let sy = (m.yy * m.yy + m.xy * m.xy).sqrt();
                let r = num.to_radians();
                let (s, c) = r.sin_cos();
                m.xx = c * sx;
                m.yx = s * sx;
                m.xy = -s * sy;
                m.yy = c * sy;
            }
            "_width" | "_height" => {
                let b = self.bounds(id);
                let (w, h) = (b.br[0] - b.tl[0], b.br[1] - b.tl[1]);
                let Some(d) = self.display_of_mut(id) else { return true };
                let m = &mut d.matrix;
                if name == "_width" && w > 0.0 {
                    let cur = (m.xx * m.xx + m.yx * m.yx).sqrt().max(1e-6);
                    let k = num / w / cur;
                    m.xx *= k;
                    m.yx *= k;
                } else if name == "_height" && h > 0.0 {
                    let cur = (m.yy * m.yy + m.xy * m.xy).sqrt().max(1e-6);
                    let k = num / h / cur;
                    m.yy *= k;
                    m.xy *= k;
                }
            }
            _ => return false,
        }
        true
    }

    fn call_native(&mut self, n: Native, this: Value, args: Vec<Value>) -> Result<Value> {
        let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
        let num = |p: &Player, i: usize| p.to_number(&args.get(i).cloned().unwrap_or_default());
        let this_id = this.as_obj();
        Ok(match n {
            Native::GotoAndStop | Native::GotoAndPlay => {
                if let Some(t) = this_id {
                    self.goto(t, arg(0), n == Native::GotoAndPlay);
                }
                Value::Undefined
            }
            Native::Play | Native::Stop => {
                if let Some(s) = this_id.and_then(|t| self.sprite_mut(t)) {
                    s.playing = n == Native::Play;
                }
                Value::Undefined
            }
            Native::NextFrame | Native::PrevFrame => {
                if let Some(t) = this_id {
                    let cur = self.sprite(t).map(|s| s.frame).unwrap_or(1);
                    let f = if n == Native::NextFrame { cur + 1 } else { cur.saturating_sub(1).max(1) };
                    self.goto(t, Value::Num(f as f64), false);
                }
                Value::Undefined
            }
            Native::SwfWait => {
                let ms = num(self, 0);
                let until = self.time + ms / 1000.0;
                if let Some(s) = this_id.and_then(|t| self.sprite_mut(t)) {
                    s.playing = false;
                    s.wait_until = Some(until);
                }
                Value::Undefined
            }
            Native::SwfTriggeredPause => {
                if let Some(s) = this_id.and_then(|t| self.sprite_mut(t)) {
                    s.playing = false;
                }
                Value::Undefined
            }
            Native::Tween => {
                if let Some(t) = this_id {
                    let prop = self.to_string(&arg(0));
                    let from = num(self, 1);
                    let to = num(self, 2);
                    let dur = num(self, 3);
                    let easing = Easing::parse(&self.to_string(&arg(4)));
                    let delay = if args.len() > 5 { num(self, 5) } else { 0.0 };
                    self.add_tween(t, &prop, from, to, dur, easing, delay.max(0.0));
                }
                Value::Undefined
            }
            Native::RemoveTweens => {
                if let Some(t) = this_id {
                    if args.is_empty() || arg(0).is_undefined() {
                        self.tweens.retain(|w| w.target != t);
                    } else {
                        let p = self.to_string(&arg(0));
                        self.tweens.retain(|w| !(w.target == t && w.prop.eq_ignore_ascii_case(&p)));
                    }
                }
                Value::Undefined
            }
            Native::SwapDepths | Native::DuplicateMovieClip | Native::RemoveMovieClip => Value::Undefined,
            Native::ToString => match this_id {
                Some(t) if self.sprite(t).is_some() => Value::str(&self.target_path(t)),
                Some(t) => self.text(t).map(|x| Value::str(&x.text)).unwrap_or_default(),
                None => Value::Undefined,
            },
            Native::CalcNumLines => Value::Num(1.0),
            Native::ArrayPush | Native::ArrayUnshift => {
                if let Some(Kind::Array(items)) = this_id.map(|t| &mut self.obj_mut(t).kind) {
                    if n == Native::ArrayPush {
                        items.extend(args.iter().cloned());
                    } else {
                        for (i, a) in args.iter().enumerate() {
                            items.insert(i, a.clone());
                        }
                    }
                    Value::Num(items.len() as f64)
                } else {
                    Value::Undefined
                }
            }
            Native::ArrayPop | Native::ArrayShift => {
                if let Some(Kind::Array(items)) = this_id.map(|t| &mut self.obj_mut(t).kind) {
                    if items.is_empty() {
                        Value::Undefined
                    } else if n == Native::ArrayPop {
                        items.pop().unwrap_or_default()
                    } else {
                        items.remove(0)
                    }
                } else {
                    Value::Undefined
                }
            }
            Native::ArrayJoin => {
                let sep = if args.is_empty() { ",".to_string() } else { self.to_string(&arg(0)) };
                let items = match this_id.map(|t| &self.obj(t).kind) {
                    Some(Kind::Array(items)) => items.clone(),
                    _ => Vec::new(),
                };
                let parts: Vec<String> = items.iter().map(|v| self.to_string(v)).collect();
                Value::str(&parts.join(&sep))
            }
            Native::ArraySplice => {
                let start = num(self, 0).max(0.0) as usize;
                let count = if args.len() > 1 { num(self, 1).max(0.0) as usize } else { usize::MAX };
                let insert: Vec<Value> = args.iter().skip(2).cloned().collect();
                let removed = if let Some(Kind::Array(items)) = this_id.map(|t| &mut self.obj_mut(t).kind) {
                    let s = start.min(items.len());
                    let e = s.saturating_add(count).min(items.len());
                    items.splice(s..e, insert).collect()
                } else {
                    Vec::new()
                };
                Value::Obj(self.new_array(removed))
            }
            Native::PlaySound => {
                let s = self.to_string(&arg(0));
                self.host.sounds.push(s);
                Value::Undefined
            }
            Native::StopSounds | Native::PrecacheSound | Native::PrecacheFontFxMaterial | Native::Noop | Native::SetCVarInteger => Value::Undefined,
            Native::GetPlatform | Native::GetTruePlatform => Value::Num(2.0),
            Native::GetCVarInteger => Value::Num(0.0),
            Native::IsJapanese => Value::Bool(false),
            Native::GetLocalString => {
                let s = self.to_string(&arg(0));
                Value::str(&self.localize(&s))
            }
            Native::StrReplace => {
                let s = self.to_string(&arg(0));
                let a = self.to_string(&arg(1));
                let b = self.to_string(&arg(2));
                Value::str(&if a.is_empty() { s } else { s.replace(&a, &b) })
            }
            Native::ToUpper => {
                let s = self.to_string(&arg(0));
                Value::str(&s.to_uppercase())
            }
            Native::Acos => Value::Num(num(self, 0).acos()),
            Native::Cos => Value::Num(num(self, 0).cos()),
            Native::Sin => Value::Num(num(self, 0).sin()),
            Native::Round => Value::Num(num(self, 0).round()),
            Native::Pow => Value::Num(num(self, 0).powf(num(self, 1))),
            Native::Sqrt => Value::Num(num(self, 0).sqrt()),
            Native::Abs => Value::Num(num(self, 0).abs()),
            Native::Floor => Value::Num(num(self, 0).floor()),
            Native::Ceil => Value::Num(num(self, 0).ceil()),
            Native::Rand => {
                let r = self.random();
                if args.is_empty() { Value::Num(r) } else { Value::Num((r * num(self, 0)).floor()) }
            }
            Native::ArrayCtor => Value::Obj(self.new_array(args)),
            Native::ObjectCtor => Value::Obj(self.new_object()),
        })
    }

    // ---------------------------------------------------------------- conversions

    pub fn to_primitive(&mut self, v: &Value) -> Value {
        match v {
            Value::Obj(o) => match &self.obj(*o).kind {
                Kind::Text(t) => Value::str(&t.text),
                Kind::Sprite(_) => Value::str(&self.target_path(*o)),
                _ => {
                    let f = self.get_member(v.clone(), "valueOf");
                    if f.as_obj().is_some() {
                        if let Ok(r) = self.call_value(f, v.clone(), Vec::new()) {
                            if !matches!(r, Value::Obj(_)) {
                                return r;
                            }
                        }
                    }
                    Value::str(&self.to_string(v))
                }
            },
            other => other.clone(),
        }
    }

    pub fn to_number(&self, v: &Value) -> f64 {
        match v {
            Value::Num(n) => *n,
            Value::Bool(b) => *b as i32 as f64,
            Value::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    i64::from_str_radix(h, 16).map(|x| x as f64).unwrap_or(f64::NAN)
                } else {
                    t.parse::<f64>().unwrap_or(f64::NAN)
                }
            }
            Value::Obj(o) => match &self.obj(*o).kind {
                Kind::Text(t) => t.text.trim().parse().unwrap_or(f64::NAN),
                _ => f64::NAN,
            },
            Value::Undefined | Value::Null => 0.0,
        }
    }

    pub fn to_bool(&self, v: &Value) -> bool {
        match v {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
            Value::Undefined | Value::Null => false,
        }
    }

    pub fn to_string(&self, v: &Value) -> String {
        match v {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => num_to_string(*n),
            Value::Str(s) => s.to_string(),
            Value::Obj(o) => match &self.obj(*o).kind {
                Kind::Sprite(_) => self.target_path(*o),
                Kind::Text(t) => t.text.clone(),
                Kind::Array(items) => items.iter().map(|i| self.to_string(i)).collect::<Vec<_>>().join(","),
                Kind::Function(_) | Kind::Native(_) => "[type Function]".into(),
                Kind::Plain => "[object Object]".into(),
            },
        }
    }

    fn abstract_equals(&mut self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Num(x), Value::Num(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => x == y,
            (Value::Obj(_), _) | (_, Value::Obj(_)) => {
                let (pa, pb) = (self.to_primitive(a), self.to_primitive(b));
                if matches!(pa, Value::Obj(_)) || matches!(pb, Value::Obj(_)) {
                    return false;
                }
                self.abstract_equals(&pa, &pb)
            }
            _ => self.to_number(a) == self.to_number(b),
        }
    }

    /// Local bounds of a sprite's current display list (shape bounds only, untransformed by its own matrix).
    pub fn bounds(&self, id: ObjId) -> crate::Rect {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        let mut add = |p: [f32; 2]| {
            lo[0] = lo[0].min(p[0]);
            lo[1] = lo[1].min(p[1]);
            hi[0] = hi[0].max(p[0]);
            hi[1] = hi[1].max(p[1]);
        };
        if let Some(s) = self.sprite(id) {
            for d in &s.display {
                let r = match (d.inst, self.swf.dict.get(d.character as usize)) {
                    (Some(i), _) if self.sprite(i).is_some() => self.bounds(i),
                    (_, Some(DictEntry::Shape(sh) | DictEntry::Morph(sh))) => sh.start_bounds,
                    (_, Some(DictEntry::EditText(t))) => t.bounds,
                    _ => continue,
                };
                if r.tl[0] > r.br[0] {
                    continue;
                }
                for p in [r.tl, [r.br[0], r.tl[1]], [r.tl[0], r.br[1]], r.br] {
                    add(d.matrix.transform(p));
                }
            }
        }
        if lo[0] > hi[0] {
            return crate::Rect::default();
        }
        crate::Rect { tl: lo, br: hi }
    }
}

fn strict_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => x == y,
        _ => false,
    }
}

fn to_i32(n: f64) -> i32 {
    if n.is_finite() { n as i64 as i32 } else { 0 }
}

pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.15}", n);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        // Keep at most 15 significant digits like Flash.
        let v: f64 = s.parse().unwrap_or(n);
        format!("{}", v)
    }
}

/// GetProperty / SetProperty index → property name.
fn property_name(i: i32) -> &'static str {
    match i {
        0 => "_x",
        1 => "_y",
        2 => "_xscale",
        3 => "_yscale",
        4 => "_currentframe",
        5 => "_totalframes",
        6 => "_alpha",
        7 => "_visible",
        8 => "_width",
        9 => "_height",
        10 => "_rotation",
        11 => "_target",
        12 => "_framesloaded",
        13 => "_name",
        14 => "_droptarget",
        15 => "_url",
        16 => "_highquality",
        17 => "_focusrect",
        18 => "_soundbuftime",
        19 => "_quality",
        20 => "_xmouse",
        21 => "_ymouse",
        _ => "",
    }
}
