//! `.entities` map entity lists (`maps/<map>.entities`, text).
//!
//! ```text
//! Version 5
//! entity {
//!     layers { "layer/..." }            // optional
//!     entityDef <name> {
//!         inherit = "<entityDef decl>";  class = "<idClass>";  expandInheritance = false; ...
//!         edit = { spawnPosition = { x = ..; } spawnOrientation = { mat = { mat[0] = { .. } } } ... }
//!     }
//! }
//! ```
//! The body uses decl syntax with three extras handled here: typed keys (`"bool" noFlood = true;`,
//! the type is dropped), override markers (`renderModelInfo = ! { ... }`, the `!` is dropped) and
//! bare strings in lists (`layers { "a" "b" }`).
//!
//! With `expandInheritance = false` the `edit` block only holds values that differ from the
//! inherited ones, down to single vector/matrix components: a missing component keeps the class
//! default (zero position, identity orientation, unit scale). [`Entity::edit_merged`] layers the
//! block over the entityDef decl chain.

use anyhow::{Result, bail, ensure};

use crate::decl::{Block, Value};

#[derive(Debug, Clone)]
pub struct Entity {
    pub name: String,
    pub layers: Vec<String>,
    /// The entityDef body (`inherit`, `class`, `edit`, ...).
    pub def: Block,
}

#[derive(Debug, Clone)]
pub struct EntitiesFile {
    pub version: u32,
    pub entities: Vec<Entity>,
}

impl Entity {
    pub fn inherit(&self) -> Option<&str> {
        self.def.str("inherit")
    }
    pub fn class(&self) -> Option<&str> {
        self.def.str("class")
    }
    pub fn edit(&self) -> Option<&Block> {
        self.def.block("edit")
    }

    /// `edit` layered over the entityDef chain's `edit` (`decls` resolves `entitydef/<inherit>`).
    pub fn edit_merged(&self, decls: Option<&crate::decldb::DeclDb>) -> Block {
        let own = self.edit().cloned().unwrap_or_default();
        let base = match (decls, self.inherit()) {
            (Some(db), Some(inh)) if !inh.is_empty() => db.get("entitydef", inh).ok().and_then(|b| b.block("edit").cloned()),
            _ => None,
        };
        match base {
            Some(b) => b.merged_with(&own),
            None => own,
        }
    }

    /// `edit.spawnPosition` (missing components 0).
    pub fn origin(&self) -> [f32; 3] {
        vec3(self.edit().and_then(|e| e.block("spawnPosition")), [0.0; 3])
    }

    /// `edit.spawnOrientation.mat` rows (idMat3, row i = axis i); missing entries keep identity.
    pub fn axis(&self) -> [[f32; 3]; 3] {
        axis(self.edit().and_then(|e| e.block("spawnOrientation")))
    }

    /// `edit.renderModelInfo.model`, from `edit` (which may come from [`Self::edit_merged`]).
    pub fn render_model(edit: &Block) -> Option<&str> {
        edit.str("renderModelInfo.model").filter(|s| !s.is_empty())
    }

    /// `edit.renderModelInfo.scale` (missing components 1).
    pub fn render_scale(edit: &Block) -> [f32; 3] {
        vec3(edit.block("renderModelInfo.scale"), [1.0; 3])
    }
}

/// `edit.bindInfo`: the entity follows `parent` (and turns with it when `oriented`).
#[derive(Debug, Clone, PartialEq)]
pub struct BindInfo {
    pub parent: String,
    pub oriented: bool,
}

/// One `moverExtender` move command (`edit.superScriptObjects.item[i]` with
/// `object = "moverExtender"`, `moveCommands.item[n]`). Missing fields are 0 / false / "".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MoveCommand {
    /// moverInfo
    pub speed: f32,
    pub time: f32,
    pub accel: f32,
    pub decel: f32,
    /// Entity whose position (and axis, with `align_to_position`) is the destination; empty = relative move.
    pub position: String,
    pub align_to_position: bool,
    pub direction: [f32; 3],
    pub rotation: [f32; 3],
    pub rotate_once: bool,
    pub delay: f32,
    pub wait_for_next_activate: bool,
    pub wait_for_children: bool,
    pub activate_on_start: String,
    pub activate_on_arrival: String,
    pub activate_on_next: String,
    pub sound_on_move: String,
    pub sound_on_arrive: String,
}

impl Entity {
    pub fn bind(edit: &Block) -> Option<BindInfo> {
        let b = edit.block("bindInfo")?;
        let parent = b.str("bindParent").filter(|p| !p.is_empty())?.to_string();
        Some(BindInfo { parent, oriented: b.get("bindOriented").and_then(Value::as_bool).unwrap_or(false) })
    }

    /// The move commands of every `moverExtender` script object, in order.
    pub fn move_commands(edit: &Block) -> Vec<MoveCommand> {
        let mut out = Vec::new();
        let Some(objs) = edit.block("superScriptObjects") else { return out };
        for (k, v) in &objs.items {
            let Some(obj) = v.as_block().filter(|_| k.starts_with("item[")) else { continue };
            if obj.str("object") != Some("moverExtender") {
                continue;
            }
            let Some(cmds) = obj.block("moveCommands") else { continue };
            for (k, v) in &cmds.items {
                let Some(c) = v.as_block().filter(|_| k.starts_with("item[")) else { continue };
                let f = |p: &str| c.f32(p).unwrap_or(0.0);
                let b = |p: &str| c.get(p).and_then(Value::as_bool).unwrap_or(false);
                let s = |p: &str| c.str(p).filter(|v| *v != "NULL").unwrap_or("").to_string();
                out.push(MoveCommand {
                    speed: f("moverInfo.speed"),
                    time: f("moverInfo.time"),
                    accel: f("moverInfo.accel"),
                    decel: f("moverInfo.decel"),
                    position: s("position"),
                    align_to_position: b("alignToPosition"),
                    direction: vec3(c.block("direction"), [0.0; 3]),
                    rotation: vec3(c.block("rotation"), [0.0; 3]),
                    rotate_once: b("rotateOnce"),
                    delay: f("delay"),
                    wait_for_next_activate: b("waitForNextActivate"),
                    wait_for_children: b("waitForChildrenBeforeMove"),
                    activate_on_start: s("activateOnStart"),
                    activate_on_arrival: s("activateOnArrival"),
                    activate_on_next: s("activateOnNext"),
                    sound_on_move: s("soundOnMove"),
                    sound_on_arrive: s("soundOnArrive"),
                });
            }
        }
        out
    }
}

/// x/y/z keys of a block over `default`.
pub fn vec3(b: Option<&Block>, default: [f32; 3]) -> [f32; 3] {
    let mut v = default;
    if let Some(b) = b {
        for (i, k) in ["x", "y", "z"].iter().enumerate() {
            if let Some(f) = b.f32(k) {
                v[i] = f;
            }
        }
    }
    v
}

/// An orientation block `{ mat = { mat[0] = {x y z} mat[1] = .. mat[2] = .. } }` over identity.
pub fn axis(b: Option<&Block>) -> [[f32; 3]; 3] {
    let mut m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if let Some(mat) = b.and_then(|b| b.block("mat")) {
        for (i, row) in m.iter_mut().enumerate() {
            *row = vec3(mat.block(&format!("mat[{i}]")), *row);
        }
    }
    m
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Eq,
    Semi,
    Bang,
    Str(&'a str),
    Atom(&'a str),
}

struct Lexer<'a> {
    src: &'a str,
    i: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Option<Tok<'a>> {
        let b = self.src.as_bytes();
        loop {
            while self.i < b.len() && b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.i + 1 < b.len() && b[self.i] == b'/' && b[self.i + 1] == b'/' {
                while self.i < b.len() && b[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            if self.i + 1 < b.len() && b[self.i] == b'/' && b[self.i + 1] == b'*' {
                self.i += 2;
                while self.i + 1 < b.len() && !(b[self.i] == b'*' && b[self.i + 1] == b'/') {
                    self.i += 1;
                }
                self.i += 2;
                continue;
            }
            break;
        }
        let c = *b.get(self.i)?;
        let t = match c {
            b'{' => Tok::Open,
            b'}' => Tok::Close,
            b'=' => Tok::Eq,
            b';' => Tok::Semi,
            b'!' => Tok::Bang,
            b'"' => {
                let start = self.i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                self.i = j.min(b.len()) + 1;
                return Some(Tok::Str(&self.src[start..j.min(b.len())]));
            }
            _ => {
                let start = self.i;
                while self.i < b.len() && !b[self.i].is_ascii_whitespace() && !matches!(b[self.i], b'{' | b'}' | b'=' | b';' | b'"') {
                    self.i += 1;
                }
                return Some(Tok::Atom(&self.src[start..self.i]));
            }
        };
        self.i += 1;
        Some(t)
    }
}

struct Parser<'a> {
    lx: Lexer<'a>,
    peeked: Option<Option<Tok<'a>>>,
}

impl<'a> Parser<'a> {
    fn peek(&mut self) -> Option<&Tok<'a>> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lx.next());
        }
        self.peeked.as_ref().unwrap().as_ref()
    }
    fn bump(&mut self) -> Option<Tok<'a>> {
        match self.peeked.take() {
            Some(t) => t,
            None => self.lx.next(),
        }
    }
    fn expect(&mut self, t: Tok) -> Result<()> {
        match self.bump() {
            Some(x) if x == t => Ok(()),
            x => bail!("expected {t:?}, found {x:?} at byte {}", self.lx.i),
        }
    }

    /// Items until the matching `}` (which is consumed).
    fn block(&mut self) -> Result<Block> {
        let mut out = Block::default();
        loop {
            match self.bump() {
                None => bail!("unterminated block"),
                Some(Tok::Close) => return Ok(out),
                Some(Tok::Semi) => {}
                Some(Tok::Open) => out.items.push((String::new(), Value::Block(self.block()?))),
                Some(Tok::Str(s)) => {
                    // Either a typed key (`"type" name = v`) or a bare list string.
                    if let Some(Tok::Atom(_)) = self.peek() {
                        let Some(Tok::Atom(k)) = self.bump() else { unreachable!() };
                        self.item(k, &mut out)?;
                    } else {
                        out.items.push((s.to_string(), Value::Atom(String::new())));
                    }
                }
                Some(Tok::Atom(k)) => self.item(k, &mut out)?,
                Some(t) => bail!("unexpected {t:?} at byte {}", self.lx.i),
            }
        }
    }

    fn item(&mut self, key: &str, out: &mut Block) -> Result<()> {
        match self.peek() {
            Some(Tok::Eq) => {
                self.bump();
                if let Some(Tok::Bang) = self.peek() {
                    self.bump();
                }
                let v = match self.bump() {
                    Some(Tok::Open) => Value::Block(self.block()?),
                    Some(Tok::Str(s)) => Value::Str(s.to_string()),
                    Some(Tok::Atom(a)) => Value::Atom(a.to_string()),
                    Some(Tok::Semi) => Value::Atom(String::new()),
                    t => bail!("unexpected {t:?} after `{key} =`"),
                };
                out.items.push((key.to_string(), v));
            }
            Some(Tok::Open) => {
                self.bump();
                let b = self.block()?;
                out.items.push((key.to_string(), Value::Block(b)));
            }
            _ => out.items.push((key.to_string(), Value::Atom(String::new()))),
        }
        Ok(())
    }
}

/// Parses a whole `.entities` file.
pub fn parse(src: &str) -> Result<EntitiesFile> {
    let mut p = Parser { lx: Lexer { src, i: 0 }, peeked: None };
    let mut version = 0;
    if let Some(Tok::Atom("Version")) = p.peek() {
        p.bump();
        match p.bump() {
            Some(Tok::Atom(v)) => version = v.parse().unwrap_or(0),
            t => bail!("bad version {t:?}"),
        }
    }
    let mut entities = Vec::new();
    while let Some(t) = p.bump() {
        ensure!(t == Tok::Atom("entity"), "expected `entity`, found {t:?} at byte {}", p.lx.i);
        p.expect(Tok::Open)?;
        let mut layers = Vec::new();
        let mut def = None;
        loop {
            match p.bump() {
                Some(Tok::Close) => break,
                Some(Tok::Atom("layers")) => {
                    p.expect(Tok::Open)?;
                    for (k, _) in p.block()?.items {
                        layers.push(k);
                    }
                }
                Some(Tok::Atom("entityDef")) => {
                    let name = match p.bump() {
                        Some(Tok::Atom(n)) | Some(Tok::Str(n)) => n.to_string(),
                        t => bail!("bad entityDef name {t:?}"),
                    };
                    p.expect(Tok::Open)?;
                    def = Some((name, p.block()?));
                }
                Some(Tok::Atom(k)) => {
                    // Unknown entity-level item: keep parsing past it.
                    let mut sink = Block::default();
                    p.item(k, &mut sink)?;
                }
                t => bail!("unexpected {t:?} in entity at byte {}", p.lx.i),
            }
        }
        let Some((name, def)) = def else { bail!("entity without entityDef at byte {}", p.lx.i) };
        entities.push(Entity { name, layers, def });
    }
    Ok(EntitiesFile { version, entities })
}

impl EntitiesFile {
    pub fn by_class<'a>(&'a self, class: &'a str) -> impl Iterator<Item = &'a Entity> + 'a {
        self.entities.iter().filter(move |e| e.class() == Some(class))
    }

    /// The `idPlayerStart` marked `initial = true`, else the first one.
    pub fn initial_player_start(&self) -> Option<&Entity> {
        let starts: Vec<&Entity> = self.by_class("idPlayerStart").collect();
        starts.iter().copied().find(|e| e.edit().and_then(|b| b.get("initial")).and_then(Value::as_bool) == Some(true)).or(starts.first().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"Version 5
entity {
	layers {
		"spawn_target_layer"
	}
	entityDef a_start {
	inherit = "player/start";
	class = "idPlayerStart";
	edit = {
		"idEntity::entityFlags_t" flags = {
			"bool" noFlood = true;
		}
		spawnPosition = { x = -17904; y = -2805; z = 3072; }
		spawnOrientation = { mat = { mat[0] = { x = -0.0000001788; y = 1; } mat[1] = { x = -0.9999999404; } } }
		renderModelInfo = ! {
			model = "models/a.lwo";
			scale = { z = 0.5; }
		}
		initial = true;
	}
}
}
"#;

    #[test]
    fn parses_mover() {
        let src = r#"Version 5
entity {
	entityDef shell {
	inherit = "func/mover_command";
	class = "idMover";
	edit = {
		superScriptObjects = {
			num = 1;
			item[0] = {
				object = "moverExtender";
				moveCommands = {
					num = 1;
					item[0] = {
						moverInfo = { speed = 0; time = 1; accel = 0; decel = 0; }
						position = "";
						alignToPosition = true;
						direction = { x = 0; y = 0; z = -256; }
						soundOnMove = "play_sfx_big_elevator_start_run";
						soundOnArrive = NULL;
						waitForNextActivate = false;
					}
				}
			}
		}
		bindInfo = { bindParent = "base"; bindOriented = true; }
	}
}
}
"#;
        let f = parse(src).unwrap();
        let edit = f.entities[0].edit().unwrap();
        let cmds = Entity::move_commands(edit);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].time, 1.0);
        assert_eq!(cmds[0].direction, [0.0, 0.0, -256.0]);
        assert!(cmds[0].align_to_position && !cmds[0].wait_for_next_activate);
        assert_eq!(cmds[0].sound_on_move, "play_sfx_big_elevator_start_run");
        assert_eq!(cmds[0].sound_on_arrive, "");
        assert_eq!(Entity::bind(edit), Some(BindInfo { parent: "base".into(), oriented: true }));
    }

    #[test]
    fn parses_entity() {
        let f = parse(SRC).unwrap();
        assert_eq!(f.version, 5);
        let e = &f.entities[0];
        assert_eq!(e.name, "a_start");
        assert_eq!(e.layers, vec!["spawn_target_layer".to_string()]);
        assert_eq!(e.class(), Some("idPlayerStart"));
        assert_eq!(e.origin(), [-17904.0, -2805.0, 3072.0]);
        let m = e.axis();
        assert_eq!(m[0], [-0.0000001788, 1.0, 0.0]);
        assert_eq!(m[1], [-0.9999999404, 1.0, 0.0]);
        assert_eq!(m[2], [0.0, 0.0, 1.0]);
        let edit = e.edit().unwrap();
        assert_eq!(Entity::render_model(edit), Some("models/a.lwo"));
        assert_eq!(Entity::render_scale(edit), [1.0, 1.0, 0.5]);
        assert_eq!(edit.block("flags").and_then(|b| b.get("noFlood")).and_then(Value::as_bool), Some(true));
        assert_eq!(f.initial_player_start().map(|e| e.name.as_str()), Some("a_start"));
    }
}
