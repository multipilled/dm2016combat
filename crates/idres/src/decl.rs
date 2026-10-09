//! Parser for idTech 6 text decls (`generated/decls/**/*.decl`).
//!
//! Grammar, as used by DOOM (2016):
//! - `key = value;` where value is a quoted string, a bare atom (number, identifier, enum) or a block
//! - `key { ... }` / `key = { ... }` blocks, optional trailing `;`
//! - `//` and `/* */` comments; a file may be a bare `{ ... }` body or `type name { ... }`
//!
//! Lookups take the last occurrence of a key, matching how later lines override earlier ones.

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Atom(String),
    Str(String),
    Block(Block),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Block {
    pub items: Vec<(String, Value)>,
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Atom(s) | Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        self.as_str().and_then(parse_number)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self.as_str()? {
            "true" => Some(true),
            "false" => Some(false),
            s => parse_number(s).map(|n| n != 0.0),
        }
    }
    pub fn as_block(&self) -> Option<&Block> {
        match self {
            Value::Block(b) => Some(b),
            _ => None,
        }
    }
}

impl Block {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Dotted path lookup, e.g. `edit.primaryFire.damage`.
    pub fn path(&self, path: &str) -> Option<&Value> {
        let mut parts = path.split('.');
        let mut cur = self.get(parts.next()?)?;
        for p in parts {
            cur = cur.as_block()?.get(p)?;
        }
        Some(cur)
    }
    pub fn f32(&self, path: &str) -> Option<f32> {
        self.path(path)?.as_f32()
    }
    pub fn str(&self, path: &str) -> Option<&str> {
        self.path(path)?.as_str()
    }
    pub fn block(&self, path: &str) -> Option<&Block> {
        self.path(path)?.as_block()
    }

    /// Recursively overlays `child` onto `self` (inheritance): child keys replace, blocks merge.
    pub fn merged_with(&self, child: &Block) -> Block {
        let mut out = self.clone();
        for (k, v) in &child.items {
            match (out.items.iter_mut().rev().find(|(ok, _)| ok == k), v) {
                (Some((_, Value::Block(base))), Value::Block(over)) => *base = base.merged_with(over),
                (Some(slot), _) => slot.1 = v.clone(),
                (None, _) => out.items.push((k.clone(), v.clone())),
            }
        }
        out
    }
}

pub fn parse_number(s: &str) -> Option<f32> {
    let s = s.trim_end_matches('f');
    if let Some(hex) = s.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f32);
    }
    s.parse::<f32>().ok()
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Eq,
    Semi,
    Str(String),
    Atom(&'a str),
}

fn lex(src: &str) -> Result<Vec<Tok<'_>>> {
    let b = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'{' => (toks.push(Tok::Open), i += 1).1,
            b'}' => (toks.push(Tok::Close), i += 1).1,
            b'=' => (toks.push(Tok::Eq), i += 1).1,
            b';' => (toks.push(Tok::Semi), i += 1).1,
            b'"' => {
                i += 1;
                let mut s = String::new();
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => '\n',
                            b't' => '\t',
                            c => c as char,
                        });
                    } else {
                        let ch = src[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8() - 1;
                    }
                    i += 1;
                }
                i += 1;
                toks.push(Tok::Str(s));
            }
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'=' | b';' | b'"') {
                    i += 1;
                }
                toks.push(Tok::Atom(&src[start..i]));
            }
        }
    }
    Ok(toks)
}

/// Parses a decl file into a top-level block. A bare `{ ... }` body is unwrapped; a
/// `type name { ... }` header becomes `type name` as the key of the single item.
pub fn parse(src: &str) -> Result<Block> {
    let toks = lex(src)?;
    let mut pos = 0;
    let top = parse_items(&toks, &mut pos, true)?;
    if top.items.len() == 1 && top.items[0].0.is_empty() {
        if let Value::Block(b) = &top.items[0].1 {
            return Ok(b.clone());
        }
    }
    Ok(top)
}

fn parse_items(toks: &[Tok], pos: &mut usize, top: bool) -> Result<Block> {
    let mut block = Block::default();
    loop {
        match toks.get(*pos) {
            None if top => return Ok(block),
            None => bail!("unterminated block"),
            Some(Tok::Close) if !top => {
                *pos += 1;
                return Ok(block);
            }
            Some(Tok::Semi) => *pos += 1,
            Some(Tok::Open) => {
                *pos += 1;
                block.items.push((String::new(), Value::Block(parse_items(toks, pos, false)?)));
            }
            Some(Tok::Atom(_) | Tok::Str(_)) => {
                // Key may be several words (`entityDef foo {`).
                let mut key = String::new();
                while let Some(Tok::Atom(a)) = toks.get(*pos) {
                    if !key.is_empty() {
                        key.push(' ');
                    }
                    key.push_str(a);
                    *pos += 1;
                }
                if let Some(Tok::Str(s)) = toks.get(*pos) {
                    if key.is_empty() {
                        key = s.clone();
                        *pos += 1;
                    }
                }
                match toks.get(*pos) {
                    Some(Tok::Eq) => {
                        *pos += 1;
                        let value = match toks.get(*pos) {
                            Some(Tok::Open) => {
                                *pos += 1;
                                Value::Block(parse_items(toks, pos, false)?)
                            }
                            Some(Tok::Str(s)) => (Value::Str(s.clone()), *pos += 1).0,
                            Some(Tok::Atom(a)) => (Value::Atom(a.to_string()), *pos += 1).0,
                            Some(Tok::Semi) => Value::Atom(String::new()),
                            t => bail!("unexpected {t:?} after `{key} =`"),
                        };
                        block.items.push((key, value));
                    }
                    Some(Tok::Open) => {
                        *pos += 1;
                        block.items.push((key, Value::Block(parse_items(toks, pos, false)?)));
                    }
                    _ => block.items.push((key, Value::Atom(String::new()))),
                }
            }
            Some(t) => bail!("unexpected token {t:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_and_inherits() {
        let base = parse(r#"{ inherit = "x"; edit = { speed = 10; fire = { dmg = 5; mode = AUTO; } } }"#).unwrap();
        let child = parse("{ edit = { fire = { dmg = 7.5; } } } // trailing").unwrap();
        let m = base.merged_with(&child);
        assert_eq!(m.f32("edit.speed"), Some(10.0));
        assert_eq!(m.f32("edit.fire.dmg"), Some(7.5));
        assert_eq!(m.str("edit.fire.mode"), Some("AUTO"));
        assert_eq!(m.str("inherit"), Some("x"));
    }

    #[test]
    fn parses_header_form() {
        let b = parse("entityDef foo/bar { /* c */ a = \"q\\\"s\"; }").unwrap();
        assert_eq!(b.block("entityDef foo/bar").unwrap().str("a"), Some("q\"s"));
    }
}
