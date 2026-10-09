//! Parser for the line-oriented decl dialect used by animWeb and md6Def decls.
//!
//! Unlike entity/weapon decls (`key = value;`, see [`crate::decl`]), these write one item per line:
//! `key`, `key value...`, `key value... {` (children until the matching `}`), where values are
//! quoted strings, bare atoms or `( n n n )` tuples. A line may also start with a quoted string
//! (`"player/fp_hands.md6" ""` in modelInfos). `//` comments run to end of line.

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Atom(String),
    Tuple(Vec<f32>),
}

impl Arg {
    pub fn text(&self) -> Option<&str> {
        match self {
            Arg::Str(s) | Arg::Atom(s) => Some(s),
            Arg::Tuple(_) => None,
        }
    }
    pub fn f32(&self) -> Option<f32> {
        crate::decl::parse_number(self.text()?)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub key: String,
    pub args: Vec<Arg>,
    pub children: Vec<Item>,
    /// True when the item opened a `{ }` block (possibly empty).
    pub block: bool,
}

impl Item {
    pub fn arg(&self, i: usize) -> Option<&Arg> {
        self.args.get(i)
    }
    /// First argument as text.
    pub fn text(&self) -> Option<&str> {
        self.args.first().and_then(Arg::text)
    }
    pub fn f32(&self) -> Option<f32> {
        self.args.first().and_then(Arg::f32)
    }
    /// The last direct child with this key (later lines override earlier ones).
    pub fn child(&self, key: &str) -> Option<&Item> {
        self.children.iter().rev().find(|c| c.key == key)
    }
    pub fn children_named<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Item> + 'a {
        self.children.iter().filter(move |c| c.key == key)
    }
    /// Dotted child path, e.g. `blendParms.sourceDuration`.
    pub fn path(&self, path: &str) -> Option<&Item> {
        let mut cur = self;
        for p in path.split('.') {
            cur = cur.child(p)?;
        }
        Some(cur)
    }
    pub fn has(&self, key: &str) -> bool {
        self.child(key).is_some()
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    Newline,
    Str(String),
    Atom(String),
    Tuple(Vec<f32>),
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let b = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => (toks.push(Tok::Newline), i += 1).1,
            b' ' | b'\t' | b'\r' | b';' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        toks.push(Tok::Newline);
                    }
                    i += 1;
                }
                i += 2;
            }
            b'{' => (toks.push(Tok::Open), i += 1).1,
            b'}' => (toks.push(Tok::Close), i += 1).1,
            b'(' => {
                let end = src[i..].find(')').map(|e| i + e).ok_or_else(|| anyhow::anyhow!("unterminated tuple"))?;
                let nums = src[i + 1..end].split_whitespace().map(|n| crate::decl::parse_number(n).ok_or_else(|| anyhow::anyhow!("bad tuple number {n:?}"))).collect::<Result<Vec<_>>>()?;
                toks.push(Tok::Tuple(nums));
                i = end + 1;
            }
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
                        i += 1;
                    } else {
                        let ch = src[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                i += 1;
                toks.push(Tok::Str(s));
            }
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'"' | b'(' | b';') {
                    if b[i] == b'/' && matches!(b.get(i + 1), Some(b'/') | Some(b'*')) {
                        break;
                    }
                    i += 1;
                }
                toks.push(Tok::Atom(src[start..i].to_string()));
            }
        }
    }
    Ok(toks)
}

/// Parses a whole decl. The result's children are the top-level items; a file that is one bare
/// `{ ... }` body is unwrapped.
pub fn parse(src: &str) -> Result<Item> {
    let toks = lex(src)?;
    let mut pos = 0;
    let mut root = Item { block: true, children: parse_items(&toks, &mut pos, true)?, ..Default::default() };
    if root.children.len() == 1 && root.children[0].key.is_empty() && root.children[0].block {
        root = root.children.pop().unwrap();
    }
    Ok(root)
}

fn parse_items(toks: &[Tok], pos: &mut usize, top: bool) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    loop {
        match toks.get(*pos) {
            None if top => return Ok(items),
            None => bail!("unterminated block"),
            Some(Tok::Newline) => *pos += 1,
            Some(Tok::Close) if !top => {
                *pos += 1;
                return Ok(items);
            }
            Some(Tok::Close) => bail!("unbalanced `}}`"),
            Some(Tok::Open) => {
                // `{` on its own line opens the previous item's block, or an anonymous one.
                *pos += 1;
                let children = parse_items(toks, pos, false)?;
                match items.last_mut() {
                    Some(prev) if !prev.block => {
                        prev.block = true;
                        prev.children = children;
                    }
                    _ => items.push(Item { block: true, children, ..Default::default() }),
                }
            }
            Some(_) => {
                let mut item = Item::default();
                match &toks[*pos] {
                    Tok::Atom(a) => item.key = a.clone(),
                    Tok::Str(s) => item.key = s.clone(),
                    Tok::Tuple(t) => item.args.push(Arg::Tuple(t.clone())),
                    _ => unreachable!(),
                }
                *pos += 1;
                loop {
                    match toks.get(*pos) {
                        Some(Tok::Atom(a)) => item.args.push(Arg::Atom(a.clone())),
                        Some(Tok::Str(s)) => item.args.push(Arg::Str(s.clone())),
                        Some(Tok::Tuple(t)) => item.args.push(Arg::Tuple(t.clone())),
                        Some(Tok::Open) => {
                            *pos += 1;
                            item.block = true;
                            item.children = parse_items(toks, pos, false)?;
                            break;
                        }
                        _ => break,
                    }
                    *pos += 1;
                }
                items.push(item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_blocks_and_tuples() {
        let src = "{\n\tprops {\n\t\tpos ( -1024 -4352 0 )\n\t\tdelta DELTA_DEFAULT\n\t}\n\tnode \"idle\" {\t// c\n\t\tedge {\n\t\t\ttoState \"x\"\n\t\t\tsourceEndRelative\n\t\t}\n\t}\n\t\"a.md6\" \"\"\t// modelIndex 0\n}\n";
        let r = parse(src).unwrap();
        assert_eq!(r.path("props.pos").unwrap().args, vec![Arg::Tuple(vec![-1024.0, -4352.0, 0.0])]);
        assert_eq!(r.path("props.delta").unwrap().text(), Some("DELTA_DEFAULT"));
        let node = r.child("node").unwrap();
        assert_eq!(node.text(), Some("idle"));
        let edge = node.child("edge").unwrap();
        assert_eq!(edge.path("toState").unwrap().text(), Some("x"));
        assert!(edge.has("sourceEndRelative"));
        assert_eq!(r.children.last().unwrap().key, "a.md6");
    }
}
