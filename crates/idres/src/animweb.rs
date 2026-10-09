//! animWeb decls (`generated/decls/animweb/**.decl`): states, scalars, per-weapon sub-webs whose
//! nodes hold blend trees (one per model) and edges with transition timings.
//!
//! This is the data only; evaluation lives in the game crate.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::ldecl::{self, Item};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Clamp,
    Repeat,
}

#[derive(Debug, Clone)]
pub struct Alias {
    /// `md6/.../x.md6anim`
    pub name: String,
    pub wrap: Wrap,
    pub rate: f32,
    /// `randomStart` (idMD6AnimProps flags bit 0).
    pub random_start: bool,
    /// `tags a b c` (tag names of the tree's tag groups; Best / Filter selection).
    pub tags: Vec<String>,
    /// `coordinate ( c... )` (blend-space position, one value per dimension).
    pub coordinate: Vec<f32>,
}

/// A tree's `tagGroup "name" { tag "t" 0|1 ... }`: tags in decl order with their default (decl) value.
#[derive(Debug, Clone, PartialEq)]
pub struct TagGroup {
    pub name: String,
    pub tags: Vec<(String, bool)>,
}

/// Index into a tree's alias list: fixed (`anim3`) or chosen by a scalar (`anim[sel]`, `anim[sel+2]`).
#[derive(Debug, Clone, PartialEq)]
pub enum AnimRef {
    Fixed(usize),
    /// `anim[var + K]`; the name keeps a `#` prefix when the decl writes one (`anim[ #select01 ]`).
    Var(String, i32),
    /// `anim[a, b]`: aliases a..=b (blend-space and filter children).
    Span(usize, usize),
}

/// A Lerp weight or rate factor: a scalar name or a literal.
#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Scalar(String),
    Const(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Anim(AnimRef),
    Lerp(Box<Expr>, Box<Expr>, Term),
    /// `expr * term`
    Scale(Box<Expr>, Term),
    /// `a, b, ...` (appears once, with an alias list shorter than the names it uses).
    List(Vec<Expr>),
    /// Any other blendEq function (case-insensitive name, lowercased): refLerp, addL / addR, subL / subR, blend /
    /// blendA, blend1 / blendY1 (blend spaces over alias coordinates), best / filter (tag selection), select /
    /// choose. Used by monster webs; the player webs only use Lerp.
    Call(String, Vec<CallArg>),
}

/// An argument of a [`Expr::Call`]: a sub-expression, or a scalar name / constant.
#[derive(Debug, Clone, PartialEq)]
pub enum CallArg {
    Expr(Expr),
    Term(Term),
}

#[derive(Debug, Clone)]
pub struct BlendTree {
    pub model_index: usize,
    pub eq_src: String,
    pub eq: Expr,
    pub anims: Vec<Alias>,
    /// Tag groups in decl order (monster webs; empty for the player webs).
    pub tag_groups: Vec<TagGroup>,
}

#[derive(Debug, Clone, Default)]
pub struct BlendParms {
    pub src_anim: String,
    pub except_src_anim: String,
    pub except_looping: bool,
    pub dest_anim: String,
    pub source_start_frame: f32,
    pub source_duration: f32,
    pub dest_start_frame: f32,
    pub dest_duration: f32,
    pub source_end_relative: bool,
    /// `BLEND_LINEAR` when absent (the web's defaultBlendType).
    pub blend_type: Option<String>,
    pub origin_blend: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub to_state: String,
    pub to_sub_web: Option<String>,
    pub weight_scale: Option<f32>,
    pub parms: BlendParms,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub state: String,
    pub delta: String,
    /// `props.customFlags` (AW_CF_PASSTHROUGH, AW_CF_IDLE_CYCLE, ...).
    pub custom_flags: Vec<String>,
    pub trees: Vec<BlendTree>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct SubWeb {
    pub name: String,
    pub hub: bool,
    pub nodes: Vec<Node>,
}

impl SubWeb {
    pub fn node(&self, state: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.state == state)
    }
}

#[derive(Debug, Clone)]
pub struct AnimWeb {
    pub model_infos: Vec<String>,
    pub states: Vec<String>,
    /// Declared scalars with their defaults, in decl order.
    pub scalars: Vec<(String, f32)>,
    pub default_blend_duration: f32,
    pub default_blend_out_window: f32,
    pub default_blend_type: String,
    pub sub_webs: Vec<SubWeb>,
}

impl AnimWeb {
    pub fn sub_web(&self, name: &str) -> Option<&SubWeb> {
        self.sub_webs.iter().find(|s| s.name == name)
    }
    pub fn scalar_defaults(&self) -> HashMap<String, f32> {
        self.scalars.iter().cloned().collect()
    }

    pub fn parse(src: &str) -> Result<Self> {
        let root = ldecl::parse(src)?;
        let props = root.child("props");
        let pf = |k: &str, d: f32| props.and_then(|p| p.child(k)).and_then(Item::f32).unwrap_or(d);
        let model_infos = props.and_then(|p| p.child("modelInfos")).map(|m| m.children.iter().map(|c| c.key.clone()).collect()).unwrap_or_default();
        let states = root.child("states").map(|s| s.children_named("state").filter_map(|c| c.text().map(str::to_string)).collect()).unwrap_or_default();
        let scalars = root
            .child("scalars")
            .map(|s| s.children.iter().map(|c| (c.key.clone(), c.f32().unwrap_or(0.0))).collect())
            .unwrap_or_default();
        let mut sub_webs = Vec::new();
        if let Some(sw) = root.child("subWebs") {
            for s in sw.children_named("subWeb") {
                let name = s.text().context("subWeb without a name")?.to_string();
                let hub = s.path("props.hub").and_then(Item::f32).unwrap_or(0.0) != 0.0;
                let nodes = s.children_named("node").map(|n| parse_node(n).with_context(|| format!("{name}: node {:?}", n.text()))).collect::<Result<Vec<_>>>()?;
                sub_webs.push(SubWeb { name, hub, nodes });
            }
        }
        Ok(AnimWeb {
            model_infos,
            states,
            scalars,
            default_blend_duration: pf("defaultBlendDuration", 3.0),
            default_blend_out_window: pf("defaultBlendOutWindow", 3.0),
            default_blend_type: props.and_then(|p| p.child("defaultBlendType")).and_then(Item::text).unwrap_or("BLEND_LINEAR").to_string(),
            sub_webs,
        })
    }
}

fn parse_node(n: &Item) -> Result<Node> {
    let state = n.text().context("node without a state")?.to_string();
    let delta = n.path("props.delta").and_then(Item::text).unwrap_or("DELTA_DEFAULT").to_string();
    let custom_flags = n.path("props.customFlags").map(|c| c.args.iter().filter_map(|a| a.text()).map(str::to_string).collect()).unwrap_or_default();
    let mut trees = Vec::new();
    if let Some(bt) = n.child("blendTrees") {
        for t in bt.children_named("tree") {
            let model_index = t.child("modelIndex").and_then(Item::f32).context("tree without modelIndex")? as usize;
            let eq_src = t.child("blendEq").and_then(Item::text).unwrap_or("anim0").to_string();
            let eq = parse_expr(&eq_src).with_context(|| format!("blendEq {eq_src:?}"))?;
            let anims = t
                .child("anims")
                .map(|a| {
                    a.children_named("alias")
                        .map(|al| Alias {
                            name: al.child("name").and_then(Item::text).unwrap_or("").to_string(),
                            wrap: match al.child("wrap").and_then(Item::text) {
                                Some("WRAP_REPEAT") => Wrap::Repeat,
                                _ => Wrap::Clamp,
                            },
                            rate: al.child("rate").and_then(Item::f32).unwrap_or(1.0),
                            random_start: al.has("randomStart"),
                            tags: al.child("tags").map(|t| t.args.iter().filter_map(|a| a.text()).map(str::to_string).collect()).unwrap_or_default(),
                            coordinate: match al.child("coordinate").and_then(|c| c.arg(0)) {
                                Some(ldecl::Arg::Tuple(v)) => v.clone(),
                                _ => Vec::new(),
                            },
                        })
                        .collect()
                })
                .unwrap_or_default();
            let tag_groups = t
                .children_named("tagGroup")
                .filter_map(|g| {
                    Some(TagGroup {
                        name: g.text()?.to_string(),
                        tags: g.children_named("tag").filter_map(|x| Some((x.text()?.to_string(), x.arg(1).and_then(|a| a.f32()).unwrap_or(0.0) != 0.0))).collect(),
                    })
                })
                .collect();
            trees.push(BlendTree { model_index, eq_src, eq, anims, tag_groups });
        }
    }
    let mut edges = Vec::new();
    if let Some(es) = n.child("edges") {
        for e in es.children_named("edge") {
            let to_state = e.child("toState").and_then(Item::text).context("edge without toState")?.to_string();
            let to_sub_web = e.child("toSubWeb").and_then(Item::text).map(str::to_string);
            let weight_scale = e.child("weightScale").and_then(Item::f32);
            let parms = e.child("blendParms").map(parse_parms).unwrap_or_default();
            edges.push(Edge { to_state, to_sub_web, weight_scale, parms });
        }
    }
    Ok(Node { state, delta, custom_flags, trees, edges })
}

fn parse_parms(p: &Item) -> BlendParms {
    let s = |k: &str| p.child(k).and_then(Item::text).unwrap_or("").to_string();
    let f = |k: &str| p.child(k).and_then(Item::f32).unwrap_or(0.0);
    BlendParms {
        src_anim: s("srcAnim"),
        except_src_anim: s("exceptSrcAnim"),
        except_looping: p.has("exceptLooping"),
        dest_anim: s("destAnim"),
        source_start_frame: f("sourceStartFrame"),
        source_duration: f("sourceDuration"),
        dest_start_frame: f("destStartFrame"),
        dest_duration: f("destDuration"),
        source_end_relative: p.has("sourceEndRelative"),
        blend_type: p.child("blendType").and_then(Item::text).map(str::to_string),
        origin_blend: p.child("originBlend").and_then(Item::text).map(str::to_string),
    }
}

// ---- blendEq expressions ----

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: u8) -> Result<()> {
        if !self.eat(c) {
            bail!("expected {:?} at {}", c as char, self.i);
        }
        Ok(())
    }
    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }
    fn number(&mut self) -> Option<f32> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || matches!(self.s[self.i], b'.' | b'-' | b'+')) {
            self.i += 1;
        }
        let v = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok();
        if v.is_none() {
            self.i = start;
        }
        v
    }
    fn term(&mut self) -> Result<Term> {
        self.ws();
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.' || *c == b'-') {
            return self.number().map(Term::Const).context("bad number");
        }
        let hash = self.eat(b'#');
        self.ident().map(|n| Term::Scalar(if hash { format!("#{n}") } else { n })).context("expected a scalar name")
    }
    fn primary(&mut self) -> Result<Expr> {
        let id = self.ident().context("expected anim or Lerp")?;
        if id.eq_ignore_ascii_case("lerp") {
            self.expect(b'(')?;
            let a = self.expr()?;
            self.expect(b',')?;
            let b = self.expr()?;
            self.expect(b',')?;
            let w = self.term()?;
            self.expect(b')')?;
            return Ok(Expr::Lerp(Box::new(a), Box::new(b), w));
        }
        if id == "anim" {
            // A bare `anim` (two MP hands webs) names the first alias.
            if !self.eat(b'[') {
                return Ok(Expr::Anim(AnimRef::Fixed(0)));
            }
            self.ws();
            // anim[a, b] span
            if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                let a = self.number().context("expected span start")? as usize;
                if self.eat(b',') {
                    let b = self.number().context("expected span end")? as usize;
                    self.expect(b']')?;
                    return Ok(Expr::Anim(AnimRef::Span(a, b)));
                }
                self.expect(b']')?;
                return Ok(Expr::Anim(AnimRef::Fixed(a)));
            }
            let hash = self.eat(b'#');
            let var = self.ident().context("expected selector scalar")?;
            let var = if hash { format!("#{var}") } else { var };
            let mut k = 0i32;
            if self.eat(b'+') {
                k = self.number().context("expected offset")? as i32;
            } else if self.eat(b'-') {
                k = -(self.number().context("expected offset")? as i32);
            }
            self.expect(b']')?;
            return Ok(Expr::Anim(AnimRef::Var(var, k)));
        }
        if let Some(n) = id.strip_prefix("anim") {
            if let Ok(i) = n.parse() {
                return Ok(Expr::Anim(AnimRef::Fixed(i)));
            }
        }
        if self.eat(b'(') {
            let mut args = Vec::new();
            if !self.eat(b')') {
                loop {
                    args.push(self.call_arg()?);
                    if self.eat(b')') {
                        break;
                    }
                    self.expect(b',')?;
                }
            }
            return Ok(Expr::Call(id.to_ascii_lowercase(), args));
        }
        bail!("unknown identifier {id}")
    }

    /// A call argument: a number or a bare scalar name is a term; anything else (anim..., f(...)) an expression.
    fn call_arg(&mut self) -> Result<CallArg> {
        self.ws();
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.' || *c == b'-') {
            return self.number().map(|n| CallArg::Term(Term::Const(n))).context("bad number");
        }
        let save = self.i;
        let id = self.ident().context("expected an argument")?;
        self.ws();
        let is_expr = id == "anim" || id.strip_prefix("anim").is_some_and(|n| n.parse::<usize>().is_ok()) || matches!(self.s.get(self.i), Some(b'(') | Some(b'['));
        self.i = save;
        if is_expr { Ok(CallArg::Expr(self.expr()?)) } else { Ok(CallArg::Term(Term::Scalar(self.ident().unwrap()))) }
    }
    fn expr(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        while self.eat(b'*') {
            e = Expr::Scale(Box::new(e), self.term()?);
        }
        Ok(e)
    }
}

pub fn parse_expr(src: &str) -> Result<Expr> {
    let mut p = P { s: src.as_bytes(), i: 0 };
    let mut list = vec![p.expr()?];
    while p.eat(b',') {
        list.push(p.expr()?);
    }
    p.ws();
    if p.i != p.s.len() {
        bail!("trailing input at {}", p.i);
    }
    Ok(if list.len() == 1 { list.pop().unwrap() } else { Expr::List(list) })
}

impl Expr {
    /// Every scalar name the expression reads.
    pub fn scalars(&self, out: &mut Vec<String>) {
        let term = |t: &Term, out: &mut Vec<String>| {
            if let Term::Scalar(s) = t {
                out.push(s.clone());
            }
        };
        match self {
            Expr::Anim(AnimRef::Var(v, _)) => out.push(v.clone()),
            Expr::Anim(_) => {}
            Expr::Lerp(a, b, w) => {
                a.scalars(out);
                b.scalars(out);
                term(w, out);
            }
            Expr::Scale(e, t) => {
                e.scalars(out);
                term(t, out);
            }
            Expr::List(l) => l.iter().for_each(|e| e.scalars(out)),
            Expr::Call(_, args) => {
                for a in args {
                    match a {
                        CallArg::Expr(e) => e.scalars(out),
                        CallArg::Term(t) => term(t, out),
                    }
                }
            }
        }
    }
}

/// `md6/.../x.md6anim` → `cooked/anim/md6/.../x.bmd6anim` (resource names are stored lowercase).
pub fn anim_resource(name: &str) -> String {
    format!("cooked/anim/{}.bmd6anim", name.trim_end_matches(".md6anim")).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blend_eqs() {
        let e = parse_expr("Lerp(Lerp(anim0*bringDownAnimRate,anim1*bringDownAnimRate,weaponLoadedBlend),anim2*bringDownAnimRate,blendInCover)").unwrap();
        let Expr::Lerp(a, _, Term::Scalar(w)) = &e else { panic!("{e:?}") };
        assert_eq!(w, "blendInCover");
        assert!(matches!(**a, Expr::Lerp(..)));
        assert_eq!(parse_expr("anim[handsVersionSelect+2]").unwrap(), Expr::Anim(AnimRef::Var("handsVersionSelect".into(), 2)));
        assert!(matches!(parse_expr("Lerp(anim0,anim1,weaponLoadedBlend)*reloadAnimRate").unwrap(), Expr::Scale(..)));
        assert!(matches!(parse_expr("anim0*shootAnimRate,anim1*shootAnimRate").unwrap(), Expr::List(_)));
        assert!(parse_expr("Lerp(Lerp(anim0, Lerp(anim1, anim2, w3), w2), anim4, zoomPCT)").is_ok());
        // Monster-web forms.
        let e = parse_expr("addR( blendy1( bodyMoveAngle, anim[ 0, 3] ), blend1( bodyAimYaw, anim[ 4, 6 ] ), 1.0 )").unwrap();
        let Expr::Call(name, args) = &e else { panic!("{e:?}") };
        assert_eq!(name, "addr");
        assert_eq!(args.len(), 3);
        assert_eq!(args[2], CallArg::Term(Term::Const(1.0)));
        assert!(parse_expr("Best( type, Filter( part, Filter( direction, Filter( startingInjury, Filter( source ) ) ) ) )").is_ok());
        assert!(parse_expr("blenda( bodyAimPitch, blend1( bodyAimYaw, anim[ 0, 4 ] ), blend1( bodyAimYaw, anim[ 5, 9 ] ) , -70, blend1( bodyAimYaw, anim[ 10,14 ] ) , 70)").is_ok());
        assert_eq!(parse_expr("anim[ #select01 ]").unwrap(), Expr::Anim(AnimRef::Var("#select01".into(), 0)));
        assert!(parse_expr("Lerp( blendy1( bodyMoveAngle, anim[0, 3]*genericAnimScale ), blendy1( bodyMoveAngle, anim[4, 7]*genericAnimScale ), woundedPercent )").is_ok());
    }
}
