//! Headless anim web runtime (idAnimator_AnimWeb + the anim stack's blend-command build and event list),
//! stepped on the game clock. Semantics from gamedata/re/ANIMWEB.md (VAs there); the renderer evaluates the
//! pose from [`AnimWebRuntime::pose_tree`].
//!
//! Decoded and followed:
//! - time: game time with 960 ticks per second; leaf clocks el = int((t - start) * rate), int frame
//!   el*fr/960 (CLAMP min N-1, REPEAT mod N-1), pose sample position (3a), SetRate keeping the frame, Restart
//!   (initCounter++, lastEventFrame -1) and BlendSetup (StartNode restarts CLAMP leaves and weightless REPEAT
//!   leaves; weightless leaves re-anchored at destFrame); every node keeps one persistent tree;
//! - blend equations: `anim[var(+K)]` select leaves (index truncated, out of range = unchanged), `X * S` rate pairs
//!   replacing the rate (S < 0 -> 1), Lerp alpha pairs (LINEAR);
//! - requests (ChangeState / ChangeStateVia / ForceState) stored and turned into a path by the next update's
//!   replan (FindPath Dijkstra with its heap/tie rules, hands edge weights); the pending edge re-checked every
//!   frame (CheckBlendWindow), interruptBlend 1 snapping an in-progress blend; edge blends with alphaRate
//!   1000/durMs (durMs = destDuration*1000/30), BlendProgress collapsing a finished root branch;
//! - the build: UpdateAlpha with dt capped at 64 ticks, GenerateBranchAlpha ease curves, leaf weights
//!   (lastTotalWeight), pruning on raw alphas;
//! - events: every leaf of the live tree, frames [from, to] inclusive (lastEventFrame rule, REPEAT wrap),
//!   CANSKIP events skipped on the minor side, fired once per (event, loop, initCounter).
//! - each model (hands, weapon) has its own anim state: an edge blends every model the destination node has a
//!   tree for; a node without a tree for a model leaves that model playing its previous tree (e.g. the plasma
//!   rifle's shootstate has no weapon tree, so the weapon keeps looping shootstate_into's fire anim);
//! - Best / Filter (md6 node types 5 / 6, ANIMWEB.md / DEMONS.md section 6): a Best acts as a select leaf whose
//!   alias is chosen from the tree's tag scalars (FUN_1415d5ea0 / FUN_1415d6ba0), re-chosen when the tag bits
//!   change; pass-through nodes (customFlags AW_CF_PASSTHROUGH) are left on the frame they are entered;
//! - blend spaces `blend[u][y]1(coord, inner)` (md6 node 3, op BLEND): aliases sorted by coordinate at Init with
//!   the 'y' mirror (0x1415d35a0 / 0x1415d39f0), candidates rebuilt through the inner filters when the tag bits
//!   change, the coordinate bracketed by the cached 1-D search (0x1415d6380 / 0x1415d1e10); `blendA(coord, base,
//!   delta, c, ...)` (node 4, op BLENDA) picks the nearest delta (0x1415d6250 / 0x1415d1d20); both resolved by the
//!   per-frame tree walk (0x1416fe500) over the current children; refLerp / addL / addR / subL / subR branches;
//!   per-op weights, pruning and minor sides (0x141a38ce0, 0x1416e8080); bracket, pick and sort checked under
//!   Unicorn (tools/animweb_space_oracle.py);
//! INTERIM: a Filter outside a Best / blend space and N-dimensional blend spaces play their first argument;
//! blend spaces start from coordinate 0 and tag bits 0 at Init; `Lerp(...) * S` applies S to every leaf below; weapon-model states of other sub-webs are dropped when
//! an edge changes sub-web; _randomN re-rolled on node entry (re-roll site not decoded); SetRate skipped when
//! the rate is unchanged; Lerp alpha scalars are not clamped (the job's clamp is not decoded).

use std::collections::HashMap;
use std::sync::Arc;

use idres::animweb::{AnimRef, AnimWeb, BlendParms, CallArg, Expr, Term, Wrap};
use idres::md6def::{AnimEvent, Md6DefDecl};
use idres::Container;

/// Anim stack time units per second (timer+0x10 = 16*60).
pub const TICKS: i32 = 960;
/// Edge durations are in 30 Hz frames regardless of an anim's own rate.
pub const EDGE_HZ: i32 = 30;
/// Most ticks one blend-command build advances branch alphas by.
pub const MAX_BUILD_DT: i32 = 64;

/// Events skipped on the minor side of a blend (eventFlags CANSKIP).
const CANSKIP: &[&str] = &["ae_sound", "ae_soundBody2", "ae_soundBody3", "ae_soundItem", "ae_leftFoot", "ae_rightFoot", "ae_legsCrossing"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimMeta {
    pub num_frames: u32,
    pub frame_rate: u32,
}

/// Anim lengths and md6Def events the runtime needs, loaded from the install.
#[derive(Default)]
pub struct AnimData {
    /// Lowercase md6anim name -> meta.
    pub meta: HashMap<String, AnimMeta>,
    /// modelIndex -> lowercase md6anim name -> events (md6Def of modelInfos[modelIndex], inherit chain merged).
    pub events: HashMap<usize, HashMap<String, Vec<AnimEvent>>>,
    /// lowercase md6 model name -> alias name -> lowercase md6anim name (md6Def `aliases`).
    pub aliases: HashMap<String, HashMap<String, String>>,
}

fn md6def_chain(c: &Container, md6: &str, depth: u32) -> Option<Md6DefDecl> {
    if depth > 16 {
        return None;
    }
    let text = String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/md6def/{md6}.decl")).ok()?).into_owned();
    let own = Md6DefDecl::parse(&text).ok()?;
    match own.inherit.clone() {
        Some(p) => Some(md6def_chain(c, &p, depth + 1).unwrap_or_default().merged_with(&own)),
        None => Some(own),
    }
}

/// Reads frame counts / rates from a .bmd6anim.
fn read_meta(c: &Container, name: &str) -> Option<AnimMeta> {
    let b = c.read_by_name(&idres::animweb::anim_resource(name)).ok()?;
    let a = idres::md6anim::Md6Anim::parse(&b).ok()?;
    Some(AnimMeta { num_frames: a.num_frames, frame_rate: a.frame_rate })
}

impl AnimData {
    /// Loads every anim of the given sub-webs and the events of every model in modelInfos.
    pub fn load(c: &Container, web: &AnimWeb, sub_webs: &[&str]) -> AnimData {
        let mut d = AnimData::default();
        for name in sub_webs {
            let Some(sw) = web.sub_web(name) else { continue };
            for n in &sw.nodes {
                for t in &n.trees {
                    for a in &t.anims {
                        let k = a.name.to_ascii_lowercase();
                        if !d.meta.contains_key(&k) {
                            if let Some(m) = read_meta(c, &a.name) {
                                d.meta.insert(k, m);
                            }
                        }
                    }
                }
            }
        }
        for (i, md6) in web.model_infos.iter().enumerate() {
            if let Some(def) = md6def_chain(c, md6, 0) {
                // Weapon models' aliases (e.g. "shoot") with their anim lengths; the hands model has 1200+
                // aliases the driver never asks for.
                if i > 0 {
                    let mut map = HashMap::new();
                    for (alias, anim) in &def.aliases {
                        let k = anim.to_ascii_lowercase();
                        if !d.meta.contains_key(&k) {
                            if let Some(m) = read_meta(c, anim) {
                                d.meta.insert(k.clone(), m);
                            }
                        }
                        map.insert(alias.clone(), k);
                    }
                    d.aliases.insert(md6.to_ascii_lowercase(), map);
                }
                d.events.insert(i, def.events.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect());
            }
        }
        d
    }
}

// ---- compiled blend trees ----

#[derive(Debug, Clone)]
enum LeafKind {
    Fixed(usize),
    /// anim[var + add]
    Select(String, i32),
    /// Best(group, Filter(...)): the alias is chosen from the tag bits.
    Best(BestTpl),
}

/// A Best node: its own group's tag mask and its filters' group masks in parse (post-) order, innermost first.
#[derive(Debug, Clone)]
struct BestTpl {
    group: u32,
    filters: Vec<u32>,
}

#[derive(Debug, Clone)]
struct LeafTpl {
    kind: LeafKind,
    rate_pairs: Vec<Term>,
}

/// idMD6Branch ops (BOP_*, ANIMWEB.md "Enums"): blendEq lerp / refLerp / addL / addR / subL / subR build branches
/// with these, a blend space is BLEND, a blendA node BLENDA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendOp {
    Lerp = 1,
    RefLerp = 2,
    AddLeft = 3,
    AddRight = 4,
    SubLeft = 5,
    SubRight = 6,
    Blend = 7,
    BlendA = 8,
}

#[derive(Debug, Clone)]
enum TNode {
    Leaf(usize),
    /// op, left, right, alpha (a scalar pair or a constant: currentAlpha = targetAlpha, alphaRate 0).
    Branch(BlendOp, usize, usize, Term),
    /// A blend space (md6 node 3) or blendA node (md6 node 4): index into `TreeTpl::spaces`; its two children are
    /// chosen by the per-frame tree walk.
    Space(usize),
}

#[derive(Debug, Clone)]
enum SpaceKind {
    /// `blend[u][y]1( coord, inner )`: BLEND between the bracketing entries of the coordinate-sorted list.
    Blend { y: bool, filters: Vec<u32> },
    /// `blendA( coord, base, delta0, c0, delta1, c1, ... )`: BLENDA of the base and the nearest delta.
    BlendA { base: usize },
}

#[derive(Debug, Clone)]
struct SpaceTpl {
    kind: SpaceKind,
    coord: Term,
    /// Blend: (node, coordinate) sorted at Init (0x1415d35a0 -> 0x1415d39f0, 'y' mirror applied).
    /// BlendA: the deltas with their coordinates in parse order.
    list: Vec<(usize, f32)>,
}

#[derive(Debug, Clone)]
struct TreeTpl {
    model_index: usize,
    /// Lowercase alias names, wraps and rates.
    aliases: Vec<(String, Wrap, f32)>,
    /// Per alias, its first `coordinate` value (0 when the alias has none).
    coords: Vec<f32>,
    /// Tag names by bit (every tag of every tag group, in decl order; 32 at most).
    tag_names: Vec<String>,
    /// Per alias, its tag mask.
    alias_bits: Vec<u32>,
    /// The tag groups' decl values (`tag "x" 1`) as a mask.
    default_bits: u32,
    /// Tag group name -> mask.
    group_masks: Vec<(String, u32)>,
    leaves: Vec<LeafTpl>,
    nodes: Vec<TNode>,
    spaces: Vec<SpaceTpl>,
    /// Per leaf, how often the StartNode functor walk (0x141725770) reaches it: branch children, every entry of a
    /// blend space's sorted list (a mirrored entry twice), a blendA's base and deltas. Each visit is one Restart.
    visits: Vec<u8>,
    root: usize,
}

fn leaf_node(t: &mut TreeTpl, kind: LeafKind) -> usize {
    t.leaves.push(LeafTpl { kind, rate_pairs: Vec::new() });
    t.nodes.push(TNode::Leaf(t.leaves.len() - 1));
    t.nodes.len() - 1
}

fn binop(name: &str) -> Option<BlendOp> {
    Some(match name {
        "reflerp" => BlendOp::RefLerp,
        "addl" => BlendOp::AddLeft,
        "addr" => BlendOp::AddRight,
        "subl" => BlendOp::SubLeft,
        "subr" => BlendOp::SubRight,
        _ => return None,
    })
}

/// `blend[u][y]<N>` (0x141a27720: after "blend" an optional 'u', an optional 'y', then atoi(rest), at least 1)
/// -> (u, y, N).
fn blend_space_name(name: &str) -> Option<(bool, bool, usize)> {
    let rest = name.strip_prefix("blend")?;
    let (u, rest) = rest.strip_prefix('u').map_or((false, rest), |r| (true, r));
    let (y, rest) = rest.strip_prefix('y').map_or((false, rest), |r| (true, r));
    let n: usize = rest.parse().ok()?;
    Some((u, y, n.max(1)))
}

/// The entries of a blend space's inner expression (0x141a24f00) and its filters, innermost first: no inner
/// expression (also inside `filter(g)`) creates one new leaf per alias, so two blend spaces over all aliases have
/// their own leaves. INTERIM: a span or `animN` uses the tree's alias leaves (the leaf parser 0x141a26110 looks
/// leaves up by anim before creating one; not traced to the end). No campaign tree uses an alias both inside and
/// outside a span, so the choice does not change any result.
fn space_entries(e: Option<&Expr>, t: &mut TreeTpl, filters: &mut Vec<u32>) -> Option<Vec<usize>> {
    let n = t.aliases.len();
    match e {
        None => Some((0..n).map(|i| leaf_node(t, LeafKind::Fixed(i))).collect()),
        Some(Expr::Anim(AnimRef::Span(a, b))) => Some(
            (*a..=(*b).min(n.saturating_sub(1)))
                .map(|i| {
                    t.nodes.push(TNode::Leaf(i));
                    t.nodes.len() - 1
                })
                .collect(),
        ),
        Some(Expr::Anim(AnimRef::Fixed(i))) if *i < n => {
            t.nodes.push(TNode::Leaf(*i));
            Some(vec![t.nodes.len() - 1])
        }
        Some(Expr::Call(f, args)) if f == "filter" => {
            let inner = args.iter().skip(1).find_map(|a| match a {
                CallArg::Expr(e) => Some(e.clone()),
                _ => None,
            });
            let out = space_entries(inner.as_ref(), t, filters)?;
            filters.push(args.first().map(|g| group_mask(t, g)).unwrap_or(0));
            Some(out)
        }
        _ => None,
    }
}

/// 0x1415d39f0 (N = 1): every entry is inserted at the lower bound of its coordinate (0x1415d5250, so before equal
/// ones); with the 'y' flag, walking inwards from both ends while either end sits at max(|first|, |last|), the end
/// with the larger |c| is mirrored (-c, same node) when the two differ; the mirrored entries are inserted the same way.
fn sort_space(entries: &[(usize, f32)], y: bool) -> Vec<(usize, f32)> {
    fn insert(out: &mut Vec<(usize, f32)>, e: (usize, f32)) {
        let i = out.partition_point(|x| x.1 < e.1);
        out.insert(i, e);
    }
    let mut out = Vec::with_capacity(entries.len() + 1);
    for &e in entries {
        insert(&mut out, e);
    }
    if y && out.len() > 1 {
        let n = out.len();
        let mx = out[0].1.abs().max(out[n - 1].1.abs());
        let mut extra = Vec::new();
        let (mut i, mut j) = (0, n - 1);
        while i < j {
            let (a, b) = (out[i].1.abs(), out[j].1.abs());
            if a != mx && b != mx {
                break;
            }
            if a != b {
                let e = if a < b { out[j] } else { out[i] };
                extra.push((e.0, -e.1));
            }
            i += 1;
            j -= 1;
        }
        for e in extra {
            insert(&mut out, e);
        }
    }
    out
}

fn compile(e: &Expr, t: &mut TreeTpl) -> usize {
    match e {
        Expr::Anim(AnimRef::Fixed(i)) => {
            let leaf = (*i).min(t.aliases.len().saturating_sub(1));
            t.nodes.push(TNode::Leaf(leaf));
        }
        Expr::Anim(AnimRef::Var(v, k)) => return leaf_node(t, LeafKind::Select(v.clone(), *k)),
        Expr::Lerp(a, b, w) => {
            let l = compile(a, t);
            let r = compile(b, t);
            t.nodes.push(TNode::Branch(BlendOp::Lerp, l, r, w.clone()));
        }
        Expr::Scale(inner, s) => {
            let n = compile(inner, t);
            let mut stack = vec![n];
            while let Some(x) = stack.pop() {
                match &t.nodes[x] {
                    TNode::Leaf(l) => {
                        let l = *l;
                        t.leaves[l].rate_pairs.push(s.clone());
                    }
                    TNode::Branch(_, a, b, _) => stack.extend([*a, *b]),
                    TNode::Space(i) => stack.extend(space_nodes(&t.spaces[*i])),
                }
            }
            return n;
        }
        Expr::List(l) => return compile(&l[0], t),
        // INTERIM: a span outside a blend space plays its first alias.
        Expr::Anim(AnimRef::Span(a, _)) => {
            let leaf = (*a).min(t.aliases.len().saturating_sub(1));
            t.nodes.push(TNode::Leaf(leaf));
        }
        Expr::Call(name, args) if name == "best" => {
            let group = args.first().map(|a| group_mask(t, a)).unwrap_or(0);
            let mut fl = Vec::new();
            for a in args.iter().skip(1) {
                if let CallArg::Expr(e) = a {
                    filter_masks(e, t, &mut fl);
                }
            }
            return leaf_node(t, LeafKind::Best(BestTpl { group, filters: fl }));
        }
        // refLerp / addL / addR / subL / subR (a, b, alpha): a branch like Lerp with that op (0x141a27720).
        Expr::Call(name, args) if binop(name).is_some() => {
            let (Some(CallArg::Expr(a)), Some(CallArg::Expr(b)), Some(CallArg::Term(w))) = (args.first(), args.get(1), args.get(2)) else {
                return compile_first(args, t);
            };
            let l = compile(a, t);
            let r = compile(b, t);
            t.nodes.push(TNode::Branch(binop(name).unwrap(), l, r, w.clone()));
        }
        Expr::Call(name, args) if blend_space_name(name).is_some_and(|(_, _, n)| n == 1) => {
            // 'u' (passed to 0x141a256f0) is used by no campaign web; its meaning is not decoded.
            let (_, y, _) = blend_space_name(name).unwrap();
            let inner = match (args.first(), args.get(1)) {
                (Some(CallArg::Term(_)), None) => None,
                (Some(CallArg::Term(_)), Some(CallArg::Expr(e))) => Some(e.clone()),
                _ => return compile_first(args, t),
            };
            let Some(CallArg::Term(coord)) = args.first().cloned() else { unreachable!() };
            let mut filters = Vec::new();
            let Some(nodes) = space_entries(inner.as_ref(), t, &mut filters) else { return compile_first(args, t) };
            let entries: Vec<(usize, f32)> = nodes
                .iter()
                .map(|&x| match &t.nodes[x] {
                    TNode::Leaf(l) => match t.leaves[*l].kind {
                        LeafKind::Fixed(i) => (x, t.coords[i]),
                        _ => (x, 0.0),
                    },
                    _ => (x, 0.0),
                })
                .collect();
            let list = sort_space(&entries, y);
            t.spaces.push(SpaceTpl { kind: SpaceKind::Blend { y, filters }, coord, list });
            t.nodes.push(TNode::Space(t.spaces.len() - 1));
        }
        Expr::Call(name, args) if name == "blenda" => {
            let (Some(CallArg::Term(coord)), Some(CallArg::Expr(base))) = (args.first(), args.get(1)) else { return compile_first(args, t) };
            let coord = coord.clone();
            let base = compile(base, t);
            let mut list = Vec::new();
            for pair in args[2..].chunks(2) {
                match pair {
                    [CallArg::Expr(d), CallArg::Term(Term::Const(c))] => {
                        let d = compile(d, t);
                        list.push((d, *c));
                    }
                    _ => return compile_first(args, t),
                }
            }
            t.spaces.push(SpaceTpl { kind: SpaceKind::BlendA { base }, coord, list });
            t.nodes.push(TNode::Space(t.spaces.len() - 1));
        }
        // INTERIM: other blendEq functions (a filter outside best / a blend space, select / choose, N-dimensional
        // blend spaces) play their first expression argument; no campaign web uses them.
        Expr::Call(_, args) => return compile_first(args, t),
    }
    t.nodes.len() - 1
}

fn compile_first(args: &[CallArg], t: &mut TreeTpl) -> usize {
    match args.iter().find_map(|a| match a {
        CallArg::Expr(e) => Some(e.clone()),
        _ => None,
    }) {
        Some(e) => compile(&e, t),
        None => {
            t.nodes.push(TNode::Leaf(0));
            t.nodes.len() - 1
        }
    }
}

/// Every node a blend space can choose from (the functor walk's order: blendA base first, then the list).
fn space_nodes(sp: &SpaceTpl) -> Vec<usize> {
    let mut v = Vec::new();
    if let SpaceKind::BlendA { base } = sp.kind {
        v.push(base);
    }
    v.extend(sp.list.iter().map(|e| e.0));
    v
}

/// StartNode's functor walk (0x141725770) over the static tree: leaf visit counts.
fn count_visits(t: &TreeTpl, x: usize, out: &mut [u8]) {
    match &t.nodes[x] {
        TNode::Leaf(l) => out[*l] = out[*l].saturating_add(1),
        TNode::Branch(_, a, b, _) => {
            count_visits(t, *a, out);
            count_visits(t, *b, out);
        }
        TNode::Space(i) => {
            for n in space_nodes(&t.spaces[*i]) {
                count_visits(t, n, out);
            }
        }
    }
}

fn group_mask(t: &TreeTpl, a: &CallArg) -> u32 {
    match a {
        CallArg::Term(Term::Scalar(g)) => t.group_masks.iter().find(|(n, _)| n == g).map(|(_, m)| *m).unwrap_or(0),
        _ => 0,
    }
}

/// Filters append themselves after parsing their inner expression (0x141a27720): innermost first.
fn filter_masks(e: &Expr, t: &TreeTpl, out: &mut Vec<u32>) {
    if let Expr::Call(n, a) = e {
        for x in a.iter().skip(1) {
            if let CallArg::Expr(inner) = x {
                filter_masks(inner, t, out);
            }
        }
        if n == "filter" {
            out.push(a.first().map(|g| group_mask(t, g)).unwrap_or(0));
        }
    }
}

fn tree_tpl(tree: &idres::animweb::BlendTree) -> TreeTpl {
    let aliases: Vec<(String, Wrap, f32)> = tree.anims.iter().map(|a| (a.name.to_ascii_lowercase(), a.wrap, a.rate)).collect();
    let leaves = (0..aliases.len()).map(|i| LeafTpl { kind: LeafKind::Fixed(i), rate_pairs: Vec::new() }).collect();
    // Tags are numbered over all groups in decl order (0x141a28e60).
    let mut tag_names: Vec<String> = Vec::new();
    let mut default_bits = 0u32;
    let mut group_masks = Vec::new();
    for g in &tree.tag_groups {
        let mut m = 0u32;
        for (name, def) in &g.tags {
            let bit = tag_names.len();
            if bit < 32 {
                m |= 1 << bit;
                if *def {
                    default_bits |= 1 << bit;
                }
            }
            tag_names.push(name.clone());
        }
        group_masks.push((g.name.clone(), m));
    }
    let alias_bits = tree
        .anims
        .iter()
        .map(|a| a.tags.iter().filter_map(|x| tag_names.iter().position(|n| n == x)).filter(|&b| b < 32).fold(0u32, |m, b| m | 1 << b))
        .collect();
    // INTERIM: an alias without `coordinate` sits at 0 (the idMD6AnimProps default is not checked; every alias of a
    // campaign blend space has one).
    let coords = tree.anims.iter().map(|a| a.coordinate.first().copied().unwrap_or(0.0)).collect();
    let mut t = TreeTpl {
        model_index: tree.model_index,
        aliases,
        coords,
        tag_names,
        alias_bits,
        default_bits,
        group_masks,
        leaves,
        nodes: Vec::new(),
        spaces: Vec::new(),
        visits: Vec::new(),
        root: 0,
    };
    if t.aliases.is_empty() {
        return t;
    }
    t.root = compile(&tree.eq, &mut t);
    let mut visits = vec![0u8; t.leaves.len()];
    count_visits(&t, t.root, &mut visits);
    t.visits = visits;
    t
}

/// Filter (0x1415d6ba0): exclude aliases by group `g` against the current bits, else against the default bits,
/// else leave the set unchanged; a pass only sticks if some alias survives it.
fn filter_pass(g: u32, cur: u32, def: u32, bits: &[u32], excl: &mut [bool]) {
    for attempt in [cur, def] {
        let active = g & attempt;
        let saved = excl.to_vec();
        let mut survivor = false;
        for (i, &b) in bits.iter().enumerate() {
            if excl[i] {
                continue;
            }
            let out = if active == 0 { g & b != 0 } else { b & active == 0 };
            if out {
                excl[i] = true;
            } else {
                survivor = true;
            }
        }
        if survivor {
            return;
        }
        excl.copy_from_slice(&saved);
    }
}

/// BestLeaf selection (0x1415d5ea0): filters (innermost first), then the Best's own group, then the highest
/// 100 * matches(current bits) + matches(default bits) over 32 bits; ties pick at random with the anim LCG
/// (*(0x1436590f0)+8: seed * 0x19660d + 0x3c6ef35f, (seed >> 10) & 0x7fff).
fn best_select(t: &TreeTpl, b: &BestTpl, cur: u32, rng: &mut u32) -> Option<usize> {
    let n = t.alias_bits.len().min(256);
    let mut excl = vec![false; n];
    for &g in b.filters.iter().chain(std::iter::once(&b.group)) {
        filter_pass(g, cur, t.default_bits, &t.alias_bits[..n], &mut excl);
    }
    let mut best = 0u32;
    let mut cands: Vec<usize> = Vec::new();
    for i in 0..n {
        if excl[i] {
            continue;
        }
        let s = (!(cur ^ t.alias_bits[i])).count_ones() * 100 + (!(t.default_bits ^ t.alias_bits[i])).count_ones();
        if s > best {
            best = s;
            cands.clear();
        }
        if s >= best {
            cands.push(i);
        }
    }
    if cands.is_empty() {
        return None;
    }
    *rng = rng.wrapping_mul(0x19660d).wrapping_add(0x3c6ef35f);
    Some(cands[((*rng >> 10) & 0x7fff) as usize % cands.len()])
}

/// The build's weight push (0x141a38ce0): left child then right child, every child visited (pruning only drops
/// commands), each leaf storing its weight in lastTotalWeight; a leaf on both sides keeps the right side's.
fn weigh_tree(tr: &mut TreeInst, x: usize, w: f32, scalar: &dyn Fn(&str) -> f32) {
    if let TNode::Leaf(l) = tr.tpl.nodes[x] {
        tr.leaves[l].last_total_weight = w;
        return;
    }
    let Some((op, a, b, c)) = tr.kids(x, scalar) else { return };
    let (wl, wr) = op_weights(op, w, c);
    if let Some(a) = a {
        weigh_tree(tr, a, wl, scalar);
    }
    if let Some(b) = b {
        weigh_tree(tr, b, wr, scalar);
    }
}

// ---- leaf clocks ----

#[derive(Debug, Clone)]
struct Leaf {
    alias: Option<usize>,
    wrap: Wrap,
    rate: f32,
    start: i32,
    /// curFrame of the previous event build; -1 after a restart.
    last_event_frame: i64,
    /// initCounter (u8, +0x1e): bumped by every Restart; part of the event dedupe key, so two leaves playing
    /// the same anim with equal counters fire an event once, as in the engine.
    init_counter: u8,
    /// Weight from the last blend-command build that reached this leaf.
    last_total_weight: f32,
    /// Best leaves: the tag bits of the last selection.
    best_bits: Option<u32>,
}

impl Leaf {
    fn elapsed(&self, t: i32) -> i64 {
        if t < self.start { 0 } else { ((t - self.start) as f32 * self.rate) as i64 }
    }
    /// GetFrame int (0x1415d3140).
    fn int_frame(&self, t: i32, m: AnimMeta) -> i64 {
        let fi = self.elapsed(t) * m.frame_rate as i64 / TICKS as i64;
        let n = m.num_frames.max(1) as i64;
        match self.wrap {
            Wrap::Clamp => fi.min(n - 1),
            Wrap::Repeat if n > 1 => fi % (n - 1),
            Wrap::Repeat => 0,
        }
    }
    fn loop_count(&self, t: i32, m: AnimMeta) -> i64 {
        let n = m.num_frames as i64;
        if self.wrap != Wrap::Repeat || n < 2 {
            return 0;
        }
        (m.frame_rate as i64 * self.elapsed(t) / TICKS as i64) / (n - 1)
    }
    fn is_playing(&self, t: i32, m: AnimMeta) -> bool {
        self.wrap == Wrap::Repeat || self.int_frame(t, m) < m.num_frames as i64 - 1
    }
    /// Pose sample position (0x141a38670): (frame, frac).
    fn sample_pos(&self, t: i32, m: AnimMeta) -> (u32, f32) {
        let el = self.elapsed(t).max(0) as u64;
        let fr = m.frame_rate as u64;
        let n = m.num_frames.max(1);
        let f = (1.0f32 / TICKS as f32) * el as f32 * fr as f32;
        match self.wrap {
            Wrap::Clamp => {
                let loops = if n > 1 { (el * fr / TICKS as u64) / (n as u64 - 1) } else { 1 };
                let f = if loops == 0 { f } else { (n - 1) as f32 };
                if f < 0.0 {
                    (0, 0.0)
                } else if f < n as f32 {
                    (f as u32, f - (f as u32) as f32)
                } else {
                    (n - 1, 0.0)
                }
            }
            Wrap::Repeat => {
                let u = f as u32;
                let frac = f - u as f32;
                (if n > 1 { u % (n - 1) } else { 0 }, frac)
            }
        }
    }
    /// SetRate (0x1415d5a20) keeping the current frame.
    fn set_rate(&mut self, t: i32, new: f32) {
        let new = if new < 0.0 { 1.0 } else { new };
        if new == self.rate {
            return;
        }
        let el = if t >= self.start { (t - self.start) as f32 * self.rate } else { 0.0 };
        self.rate = new;
        self.start = if new > 0.0 { (t as f32 - el / new) as i32 } else { t };
    }
    /// Restart (0x1415d4e50) at `frame`.
    fn restart(&mut self, t: i32, frame: i32, wrap: Wrap, m: Option<AnimMeta>) {
        self.init_counter = self.init_counter.wrapping_add(1);
        self.wrap = wrap;
        self.anchor(t, frame, m);
        self.last_event_frame = -1;
    }
    /// Shows `frame` at time `t`.
    fn anchor(&mut self, t: i32, frame: i32, m: Option<AnimMeta>) {
        let fr = m.map(|m| m.frame_rate.max(1) as i32).unwrap_or(30);
        let r = if self.rate > 0.0 { self.rate } else { 1.0 };
        self.start = (t as f32 - ((frame * TICKS) / fr) as f32 / r) as i32;
    }
}

#[derive(Debug, Clone)]
struct TreeInst {
    tpl: Arc<TreeTpl>,
    leaves: Vec<Leaf>,
    spaces: Vec<SpaceInst>,
}

/// A blend space's / blendA's per-frame state (md6 node fields in the comments).
#[derive(Debug, Clone, Default)]
struct SpaceInst {
    /// Children (+0x10 / +0x18) and currentAlpha (+0x2c).
    left: Option<usize>,
    right: Option<usize>,
    alpha: f32,
    /// Coordinate of the last resolve (+0xb0 / blendA +0x74).
    prev: f32,
    /// Tag bits of the last resolve (+0x134).
    last_bits: u32,
    /// Candidates after the filters (+0x70 / +0x78), rebuilt when the tag bits change.
    cands: Vec<(usize, f32)>,
    /// Cached bracket (+0xc8, two indices): valid after a resolve computed one, dropped by a rebuild.
    bracket: Option<(usize, usize)>,
}

impl TreeInst {
    fn new(tpl: Arc<TreeTpl>, leaves: Vec<Leaf>) -> Self {
        let mut tr = TreeInst { spaces: vec![SpaceInst::default(); tpl.spaces.len()], tpl, leaves };
        // Init (0x1415d35a0 -> 0x1415d6380(node, 1, 1); blendA likewise): resolved once at construction with the
        // node's initial values. INTERIM: those are taken as coordinate 0 and tag bits 0 (constructor values not
        // checked); the first frame re-resolves whenever the scalars differ.
        for i in 0..tr.spaces.len() {
            tr.resolve_space(i, 0.0, 0, true);
        }
        tr
    }

    /// The per-frame tree walk (0x1416fe500): resolves the blend spaces it reaches through the CURRENT children
    /// only (branches, blend spaces and blendA nodes recurse into +0x10 and +0x18).
    fn walk(&mut self, x: usize, scalar: &dyn Fn(&str) -> f32, bits: u32) {
        let tpl = self.tpl.clone();
        match &tpl.nodes[x] {
            TNode::Leaf(_) => {}
            TNode::Branch(_, a, b, _) => {
                self.walk(*a, scalar, bits);
                self.walk(*b, scalar, bits);
            }
            TNode::Space(i) => {
                let v = match &tpl.spaces[*i].coord {
                    Term::Scalar(s) => scalar(s),
                    Term::Const(c) => *c,
                };
                self.resolve_space(*i, v, bits, false);
                let st = &self.spaces[*i];
                let (l, r) = (st.left, st.right);
                if let Some(l) = l {
                    self.walk(l, scalar, bits);
                }
                if let Some(r) = r {
                    self.walk(r, scalar, bits);
                }
            }
        }
    }

    /// Blend space resolve 0x1415d6380 (param_3 = 1: the sorted bracket search) / blendA resolve 0x1415d6250.
    fn resolve_space(&mut self, i: usize, x: f32, cur: u32, init: bool) {
        let tpl = self.tpl.clone();
        let sp = &tpl.spaces[i];
        let st = &mut self.spaces[i];
        let changed = (x - st.prev).abs() >= f32::MIN_POSITIVE;
        match &sp.kind {
            SpaceKind::BlendA { base } => {
                if !init && !changed {
                    return;
                }
                st.prev = x;
                st.left = Some(*base);
                if sp.list.is_empty() {
                    st.right = Some(*base);
                    st.alpha = 0.0;
                } else {
                    let (d, a) = blenda_pick(&sp.list, x);
                    st.right = Some(d);
                    st.alpha = a;
                }
            }
            SpaceKind::Blend { y, filters } => {
                let retag = !filters.is_empty() && cur != st.last_bits;
                if !init && !retag && !changed {
                    return;
                }
                if !filters.is_empty() && (init || retag) {
                    // Exclusion over the Init-sorted list (filters innermost first, as for Best), then the
                    // survivors: the node list keeps each node once (AddUnique) while every surviving coordinate is
                    // appended, and the re-sort pairs them by position (0x1415d39f0 iterates the node count).
                    let bits: Vec<u32> = sp.list.iter().map(|e| node_alias(&tpl, e.0).map_or(0, |a| tpl.alias_bits[a])).collect();
                    let mut excl = vec![false; bits.len()];
                    for &g in filters {
                        filter_pass(g, cur, tpl.default_bits, &bits, &mut excl);
                    }
                    let mut nodes: Vec<usize> = Vec::new();
                    let mut coords: Vec<f32> = Vec::new();
                    for (k, e) in sp.list.iter().enumerate() {
                        if excl[k] {
                            continue;
                        }
                        if !nodes.contains(&e.0) {
                            nodes.push(e.0);
                        }
                        coords.push(e.1);
                    }
                    let entries: Vec<(usize, f32)> = nodes.iter().zip(&coords).map(|(n, c)| (*n, *c)).collect();
                    st.cands = sort_space(&entries, *y);
                    st.bracket = None;
                }
                st.last_bits = cur;
                st.prev = x;
                let list = if filters.is_empty() { &sp.list } else { &st.cands };
                match list.len() {
                    0 => {
                        st.left = None;
                        st.right = None;
                        st.alpha = 0.0;
                    }
                    1 => {
                        st.left = Some(list[0].0);
                        st.right = Some(list[0].0);
                        st.alpha = 0.0;
                    }
                    _ => {
                        let (lo, hi, a) = bracket(list, x, &mut st.bracket);
                        st.left = Some(list[lo].0);
                        st.right = Some(list[hi].0);
                        st.alpha = a;
                    }
                }
            }
        }
    }

    /// A node's op, children and raw alpha (currentAlpha) for this frame.
    fn kids(&self, x: usize, scalar: &dyn Fn(&str) -> f32) -> Option<(BlendOp, Option<usize>, Option<usize>, f32)> {
        match &self.tpl.nodes[x] {
            TNode::Leaf(_) => None,
            TNode::Branch(op, a, b, w) => Some((*op, Some(*a), Some(*b), match w {
                Term::Scalar(s) => scalar(s),
                Term::Const(c) => *c,
            })),
            TNode::Space(i) => {
                let st = &self.spaces[*i];
                let op = match self.tpl.spaces[*i].kind {
                    SpaceKind::Blend { .. } => BlendOp::Blend,
                    SpaceKind::BlendA { .. } => BlendOp::BlendA,
                };
                Some((op, st.left, st.right, st.alpha))
            }
        }
    }
}

/// The alias a leaf node plays when its kind is fixed.
fn node_alias(t: &TreeTpl, x: usize) -> Option<usize> {
    match &t.nodes[x] {
        TNode::Leaf(l) => match t.leaves[*l].kind {
            LeafKind::Fixed(i) => Some(i),
            _ => None,
        },
        _ => None,
    }
}

/// 1-D bracket 0x1415d1e10 (sorted path): the cached pair is reused while lo.c <= x <= hi.c; else the first
/// i >= 1 with x <= c[i] gives (i-1, i); x past the last coordinate gives (last, last), alpha 0. alpha =
/// hi.c - lo.c > 0 ? (clamp(x, lo.c, hi.c) - lo.c) / (hi.c - lo.c) : 0 (clamp: x = min(x, hi.c) first).
fn bracket(list: &[(usize, f32)], x: f32, cache: &mut Option<(usize, usize)>) -> (usize, usize, f32) {
    let alpha = |lo: f32, hi: f32| {
        if 0.0 < hi - lo {
            let mut v = x;
            if hi <= v {
                v = hi;
            }
            if v <= lo {
                v = lo;
            }
            (v - lo) / (hi - lo)
        } else {
            0.0
        }
    };
    if let Some((lo, hi)) = *cache {
        let (cl, ch) = (list[lo].1, list[hi].1);
        if cl <= x && x <= ch {
            return (lo, hi, alpha(cl, ch));
        }
    }
    for i in 1..list.len() {
        if x <= list[i].1 {
            *cache = Some((i - 1, i));
            return (i - 1, i, alpha(list[i - 1].1, list[i].1));
        }
    }
    let last = list.len() - 1;
    *cache = Some((last, last));
    (last, last, 0.0)
}

/// blendA pick 0x1415d1d20: the first local minimum of (int)|c - x| over the deltas in order (ties keep the earlier
/// one); alpha = clamp(x, min(c, 0), max(c, 0)) / c.
fn blenda_pick(list: &[(usize, f32)], x: f32) -> (usize, f32) {
    let mut sel = 0;
    if list.len() > 1 {
        let mut d = (list[0].1 - x).abs() as i32;
        for (i, e) in list.iter().enumerate().skip(1) {
            let di = (e.1 - x).abs() as i32;
            if d <= di {
                break;
            }
            d = di;
            sel = i;
        }
    }
    let c = list[sel].1;
    let (lo, hi) = if c < 0.0 { (c, 0.0) } else { (0.0, c) };
    let mut v = x;
    if hi <= v {
        v = hi;
    }
    if v <= lo {
        v = lo;
    }
    (list[sel].0, v / c)
}

/// Command-builder weights per op (0x141a38ce0): (left, right) for parent weight `w` and eased alpha `a`.
fn op_weights(op: BlendOp, w: f32, a: f32) -> (f32, f32) {
    match op {
        BlendOp::Lerp | BlendOp::Blend | BlendOp::BlendA => ((1.0 - a) * w, a * w),
        BlendOp::RefLerp | BlendOp::AddRight | BlendOp::SubRight => (1.0, 0.0),
        BlendOp::AddLeft | BlendOp::SubLeft => (0.0, 1.0),
    }
}

/// Which children emit pose commands (0x141a38ce0, filterGroup 0, raw alpha `c`): LERP / REF_LERP / BLEND prune
/// the left at c == 1 and the right at c == 0; the base of an additive op always emits, its delta only for c != 0.
fn op_emits(op: BlendOp, c: f32) -> (bool, bool) {
    match op {
        BlendOp::Lerp | BlendOp::RefLerp | BlendOp::Blend => (c != 1.0, c != 0.0),
        BlendOp::AddLeft | BlendOp::SubLeft => (c != 0.0, true),
        BlendOp::AddRight | BlendOp::SubRight | BlendOp::BlendA => (true, c != 0.0),
    }
}

#[derive(Debug, Clone)]
struct NodeInst {
    trees: Vec<TreeInst>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

/// GenerateBranchAlpha (0x141a383e0) for current alpha `c`, target `t`.
fn ease(curve: Curve, c: f32, t: f32) -> f32 {
    match curve {
        Curve::Linear => c,
        Curve::EaseIn => {
            if c <= t {
                c * c
            } else {
                2.0 * c - c * c
            }
        }
        Curve::EaseOut => {
            if c <= t {
                2.0 * c - c * c
            } else {
                c * c
            }
        }
        Curve::EaseInOut => {
            let x = c + c;
            if x < 1.0 {
                (0.5 * x) * x
            } else {
                let u = x - 1.0;
                (((u + u) - u * u) + 1.0) * 0.5
            }
        }
    }
}

type Key = (usize, usize);

/// A model's root: a node's trees, or an edge blend from an older root to a node.
#[derive(Debug, Clone)]
enum Root {
    Node(Key),
    Blend { left: Box<Root>, right: Key, cur: f32, rate: f32, curve: Curve },
}

impl Root {
    fn newest(&self) -> Key {
        match self {
            Root::Node(k) => *k,
            Root::Blend { right, .. } => *right,
        }
    }
    fn keys(&self, out: &mut Vec<Key>) {
        match self {
            Root::Node(k) => out.push(*k),
            Root::Blend { left, right, .. } => {
                left.keys(out);
                out.push(*right);
            }
        }
    }
}

/// An anim event crossed this update.
#[derive(Debug, Clone)]
pub struct FiredEvent {
    pub model_index: usize,
    pub anim: String,
    pub event: AnimEvent,
}

/// The pose to evaluate for one model, mirroring the blend-command list (pruned on raw alphas).
#[derive(Debug, Clone)]
pub enum PoseNode {
    /// Decode the anim at frame + frac.
    Leaf { anim: String, frame: u32, frac: f32 },
    /// A leaf without an anim: the bind pose.
    Bind,
    /// LERP kernel with the eased alpha (LERP, BLEND, REF_LERP). INTERIM: REF_LERP's reference-pose side output
    /// (ANIMWEB.md 3c) is not produced; no campaign blend equation uses refLerp.
    Lerp { left: Box<PoseNode>, right: Box<PoseNode>, alpha: f32 },
    /// An additive op (ADD_LEFT / ADD_RIGHT / SUB_LEFT / SUB_RIGHT / BLENDA) of the left and right pose with the
    /// eased alpha (ANIMWEB.md 3c: base = left for ADD_RIGHT / SUB_RIGHT / BLENDA, right for ADD_LEFT / SUB_LEFT).
    Op { op: BlendOp, left: Box<PoseNode>, right: Box<PoseNode>, alpha: f32 },
}

/// Window check result (CheckBlendWindow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Wait,
    Take,
    Missed,
}

#[derive(Debug, Clone)]
struct Request {
    dest_sub: usize,
    dest_state: String,
    via: Option<(usize, String)>,
    forced: Option<(Key, BlendParms)>,
    /// ForceStateVia: where the path continues after the forced edge.
    then: Option<(usize, String)>,
    interrupt_path: u8,
    interrupt_blend: u8,
}

#[derive(Debug, Clone, PartialEq)]
struct Tracked {
    model_index: usize,
    anim: String,
    event: usize,
    frame: i64,
    loop_count: i64,
    init: u8,
}

/// FindPath's 1-based binary min-heap (push sifts up while parent.key > key; pop moves the last item to the
/// top and sifts down to the smaller child, right only if strictly smaller, while child.key < item.key).
struct PathHeap {
    a: Vec<(usize, i32)>,
}

impl PathHeap {
    fn push(&mut self, node: usize, key: i32) {
        self.a.push((node, key));
        let mut i = self.a.len();
        while i > 1 && self.a[i / 2 - 1].1 > key {
            self.a.swap(i - 1, i / 2 - 1);
            i /= 2;
        }
    }
    fn pop(&mut self) -> Option<(usize, i32)> {
        if self.a.is_empty() {
            return None;
        }
        let top = self.a[0];
        let last = self.a.pop().unwrap();
        if !self.a.is_empty() {
            self.a[0] = last;
            let n = self.a.len();
            let mut i = 1;
            loop {
                let l = 2 * i;
                if l > n {
                    break;
                }
                let mut c = l;
                if l < n && self.a[l].1 < self.a[l - 1].1 {
                    c = l + 1;
                }
                if self.a[c - 1].1 < self.a[i - 1].1 {
                    self.a.swap(c - 1, i - 1);
                    i = c;
                } else {
                    break;
                }
            }
        }
        Some(top)
    }
}

pub struct AnimWebRuntime {
    pub web: Arc<AnimWeb>,
    pub data: Arc<AnimData>,
    pub scalars: HashMap<String, f32>,
    tpls: HashMap<Key, Vec<Arc<TreeTpl>>>,
    nodes: HashMap<Key, NodeInst>,
    /// Per-model anim state roots: (modelIndex, root).
    roots: Vec<(usize, Root)>,
    /// The web's current node (the newest node entered).
    cur_node: Option<Key>,
    /// Stored request, consumed into `path` by the next update's replan.
    request: Option<Request>,
    /// Remaining nodes of the current path (path[0] is the next node to enter).
    path: Vec<Key>,
    /// Caller blend parms for the first step of a forced path (ForceState).
    forced_parms: Option<BlendParms>,
    interrupt_path: u8,
    interrupt_blend: u8,
    /// idAnimWebHands::EdgeWeight (1000 into shoot / melee, else 100) instead of the base 100.
    pub hands_weights: bool,
    /// First global node index of each sub-web.
    node_base: Vec<usize>,
    tracked: Vec<Tracked>,
    /// Web events of the current update: (kind, node). 0 edge into, 1 edge out of, 2 blend-in done, 3 blend-out done.
    node_events: Vec<(u8, Key)>,
    /// Anim events of the last update (for sound / FX consumers).
    pub last_fired: Vec<FiredEvent>,
    now: i32,
    last_build: i32,
    rng: u32,
}

impl AnimWebRuntime {
    pub fn new(web: Arc<AnimWeb>, data: Arc<AnimData>) -> Self {
        // Decl scalar defaults are (int != 0) ? 1 : 0.
        let scalars = web.scalars.iter().map(|(k, v)| (k.clone(), if *v != 0.0 { 1.0 } else { 0.0 })).collect();
        let mut node_base = Vec::with_capacity(web.sub_webs.len());
        let mut acc = 0;
        for sw in &web.sub_webs {
            node_base.push(acc);
            acc += sw.nodes.len();
        }
        AnimWebRuntime {
            web,
            data,
            scalars,
            tpls: HashMap::new(),
            nodes: HashMap::new(),
            roots: Vec::new(),
            cur_node: None,
            request: None,
            path: Vec::new(),
            forced_parms: None,
            interrupt_path: 1,
            interrupt_blend: 1,
            hands_weights: true,
            node_base,
            tracked: Vec::new(),
            node_events: Vec::new(),
            last_fired: Vec::new(),
            now: 0,
            last_build: 0,
            rng: 0x1234_5678,
        }
    }

    pub fn set_scalar(&mut self, name: &str, v: f32) {
        self.scalars.insert(name.to_string(), v);
    }
    pub fn scalar(&self, name: &str) -> f32 {
        self.scalars.get(name).copied().unwrap_or(0.0)
    }
    pub fn now(&self) -> i32 {
        self.now
    }

    fn sub_index(&self, name: &str) -> Option<usize> {
        self.web.sub_webs.iter().position(|s| s.name == name)
    }
    fn node_index(&self, sub: usize, state: &str) -> Option<usize> {
        self.web.sub_webs[sub].nodes.iter().position(|n| n.state == state)
    }
    fn meta(&self, name: &str) -> Option<AnimMeta> {
        self.data.meta.get(name).copied()
    }

    /// (sub-web, state) of the newest node.
    pub fn current(&self) -> Option<(&str, &str)> {
        let (s, n) = self.cur_node?;
        let sw = &self.web.sub_webs[s];
        Some((&sw.name, &sw.nodes[n].state))
    }
    /// True while an edge blend branch is live.
    pub fn is_blending(&self) -> bool {
        self.roots.iter().any(|(m, r)| *m == 0 && matches!(r, Root::Blend { .. }))
    }
    /// True while a request or path still has edges to take.
    pub fn has_pending(&self) -> bool {
        !self.path.is_empty() || self.request.is_some()
    }
    /// The state a stored request or the pending path ends in.
    pub fn pending_target(&self) -> Option<(&str, &str)> {
        if let Some(r) = &self.request {
            return Some((&self.web.sub_webs[r.dest_sub].name, &r.dest_state));
        }
        let &(s, n) = self.path.last()?;
        let sw = &self.web.sub_webs[s];
        Some((&sw.name, &sw.nodes[n].state))
    }

    fn templates(&mut self, key: Key) -> Vec<Arc<TreeTpl>> {
        let web = self.web.clone();
        self.tpls.entry(key).or_insert_with(|| web.sub_webs[key.0].nodes[key.1].trees.iter().map(|t| Arc::new(tree_tpl(t))).collect()).clone()
    }

    /// The node's persistent tree, created on first use.
    fn ensure_node(&mut self, key: Key) {
        if self.nodes.contains_key(&key) {
            return;
        }
        let t = self.now;
        let trees = self
            .templates(key)
            .into_iter()
            .map(|tpl| {
                let leaves = tpl
                    .leaves
                    .iter()
                    .map(|l| {
                        // anim[v+K] leaves are created WRAP_REPEAT with rate 1 on alias[K] (0x141a26110 ->
                        // 0x141a24a50); fixed leaves take their alias wrap and rate.
                        let (alias, wrap, rate) = match &l.kind {
                            LeafKind::Fixed(i) => (Some(*i), tpl.aliases[*i].1, tpl.aliases[*i].2),
                            LeafKind::Select(_, k) => ((*k >= 0 && (*k as usize) < tpl.aliases.len()).then_some(*k as usize), Wrap::Repeat, 1.0),
                            // Resolved by the first apply_pairs (StartNode runs them before restarting leaves).
                            LeafKind::Best(_) => (None, Wrap::Clamp, 1.0),
                        };
                        Leaf { alias, wrap, rate, start: t, last_event_frame: -1, init_counter: 0, last_total_weight: 0.0, best_bits: None }
                    })
                    .collect();
                TreeInst::new(tpl, leaves)
            })
            .collect();
        self.nodes.insert(key, NodeInst { trees });
    }

    /// Scalar pairs: select indices, then rate pairs in parse order.
    fn apply_pairs(&mut self, key: Key, t: i32) {
        let scalars = &self.scalars;
        let rng = &mut self.rng;
        let Some(n) = self.nodes.get_mut(&key) else { return };
        let get = |s: &str| scalars.get(s).copied().unwrap_or(0.0);
        for tr in &mut n.trees {
            let tpl = tr.tpl.clone();
            for (li, lt) in tpl.leaves.iter().enumerate() {
                let leaf = &mut tr.leaves[li];
                if let LeafKind::Best(b) = &lt.kind {
                    // Tag pairs set / clear the Best's bits from the tag scalars (0x141724040); the selection is
                    // redone when they changed (0x1415d5ea0).
                    let cur = tpl.tag_names.iter().enumerate().filter(|(i, n)| *i < 32 && get(n) != 0.0).fold(0u32, |m, (i, _)| m | 1 << i);
                    if leaf.best_bits != Some(cur) {
                        leaf.best_bits = Some(cur);
                        match best_select(&tpl, b, cur, rng) {
                            Some(i) => {
                                if leaf.alias != Some(i) {
                                    leaf.alias = Some(i);
                                    leaf.set_rate(t, tpl.aliases[i].2);
                                }
                            }
                            // sel 0xffff: the node's first leaf plays.
                            None if leaf.alias.is_none() && !tpl.aliases.is_empty() => leaf.alias = Some(0),
                            None => {}
                        }
                    }
                }
                if let LeafKind::Select(var, add) = &lt.kind {
                    let idx = get(var) as i64 + *add as i64;
                    if idx >= 0 && (idx as usize) < tpl.aliases.len() {
                        // Swaps the anim and sets the alias rate; never changes wrap or restarts.
                        let i = idx as usize;
                        leaf.alias = Some(i);
                        leaf.set_rate(t, tpl.aliases[i].2);
                    }
                }
                for p in &lt.rate_pairs {
                    let r = match p {
                        Term::Scalar(s) => get(s),
                        Term::Const(c) => *c,
                    };
                    leaf.set_rate(t, r);
                }
            }
            // The tree walk (0x1416fe500) resolves the blend spaces after the pairs; their filters see the same
            // tag bits as a Best.
            if !tpl.spaces.is_empty() {
                let bits = tpl.tag_names.iter().enumerate().filter(|(i, n)| *i < 32 && get(n) != 0.0).fold(0u32, |m, (i, _)| m | 1 << i);
                tr.walk(tpl.root, &get, bits);
            }
        }
    }

    /// CheckBlendWindow's timing leaf: DFS right child before left; `except_looping` wants a clamped leaf.
    fn timing_leaf(&self, key: Key, src_anim: &str, except_src: &str, except_looping: bool) -> Option<(usize, usize)> {
        let n = self.nodes.get(&key)?;
        let ti = n.trees.iter().position(|t| t.tpl.model_index == 0).unwrap_or(0);
        let tr = n.trees.get(ti)?;
        let tpl = &tr.tpl;
        if tpl.nodes.is_empty() {
            return None;
        }
        let mut stack = vec![tpl.root];
        while let Some(x) = stack.pop() {
            match &tpl.nodes[x] {
                TNode::Leaf(l) => {
                    let leaf = &tr.leaves[*l];
                    let name = leaf.alias.map(|a| tpl.aliases[a].0.as_str()).unwrap_or("");
                    let ok = if !src_anim.is_empty() {
                        name.eq_ignore_ascii_case(src_anim)
                    } else if !except_src.is_empty() {
                        !name.eq_ignore_ascii_case(except_src)
                    } else if except_looping {
                        leaf.wrap == Wrap::Clamp && leaf.alias.is_some()
                    } else {
                        true
                    };
                    if ok {
                        return Some((ti, *l));
                    }
                }
                // Right child visited first: push left, then right.
                _ => {
                    if let Some((_, a, b, _)) = tr.kids(x, &|s| self.scalar(s)) {
                        stack.extend(a);
                        stack.extend(b);
                    }
                }
            }
        }
        None
    }

    fn check_window(&self, key: Key, p: &BlendParms) -> Window {
        // A missing timing leaf / anim is an error in the engine and never takes the edge.
        let Some((ti, li)) = self.timing_leaf(key, &p.src_anim, &p.except_src_anim, p.except_looping) else { return Window::Wait };
        let tr = &self.nodes[&key].trees[ti];
        let leaf = &tr.leaves[li];
        let Some(m) = leaf.alias.and_then(|a| self.meta(&tr.tpl.aliases[a].0)) else { return Window::Wait };
        let nf = m.num_frames as i64;
        let sff = if p.source_end_relative { nf - p.source_start_frame as i64 - 1 } else { p.source_start_frame as i64 };
        let slf = (sff + p.source_duration as i64).clamp(0, 0x7fff);
        let frame = leaf.int_frame(self.now, m);
        let last = if leaf.wrap == Wrap::Repeat { nf - 1 } else { nf };
        if sff >= last {
            // Window at/after the end: take once the anim stops playing.
            return if leaf.is_playing(self.now, m) { Window::Wait } else { Window::Take };
        }
        if frame < sff {
            Window::Wait
        } else if frame <= slf {
            Window::Take
        } else {
            Window::Missed
        }
    }

    /// Edge from `from` to `to`, first in decl order.
    fn edge_between(&self, from: Key, to: Key) -> Option<&idres::animweb::Edge> {
        let sw = &self.web.sub_webs[from.0];
        let target = &self.web.sub_webs[to.0];
        sw.nodes[from.1].edges.iter().find(|e| {
            let es = e.to_sub_web.as_deref().unwrap_or(&sw.name);
            es == target.name && e.to_state == target.nodes[to.1].state
        })
    }

    fn global(&self, n: Key) -> usize {
        self.node_base[n.0] + n.1
    }
    fn local(&self, g: usize) -> Key {
        let s = self.node_base.iter().rposition(|&b| b <= g).unwrap_or(0);
        (s, g - self.node_base[s])
    }

    /// FindPath (0x1417029e0): nodes after `from` up to the cheapest node of `dest_sub` in state `state`.
    fn find_path(&self, from: Key, dest_sub: usize, state: &str) -> Option<Vec<Key>> {
        let total: usize = self.web.sub_webs.iter().map(|s| s.nodes.len()).sum();
        let candidates: Vec<usize> = self.web.sub_webs[dest_sub].nodes.iter().enumerate().filter(|(_, n)| n.state == state).map(|(i, _)| self.global((dest_sub, i))).collect();
        if candidates.is_empty() {
            return None;
        }
        let traversable = |g: usize| {
            let (s, _) = self.local(g);
            s == from.0 || s == dest_sub || self.web.sub_webs[s].hub
        };
        let start = self.global(from);
        let mut cost = vec![i32::MAX; total];
        let mut closed = vec![false; total];
        let mut prev = vec![usize::MAX; total];
        cost[start] = 0;
        let mut heap = PathHeap { a: Vec::new() };
        heap.push(start, 0);
        let (mut pops, mut found) = (0usize, 0usize);
        while let Some((u, _)) = heap.pop() {
            pops += 1;
            if pops > total {
                break;
            }
            if closed[u] {
                continue;
            }
            closed[u] = true;
            if candidates.contains(&u) {
                found += 1;
                if found == candidates.len() {
                    break;
                }
            }
            let (us, un) = self.local(u);
            let sw = &self.web.sub_webs[us];
            for e in &sw.nodes[un].edges {
                let es = e.to_sub_web.as_deref().unwrap_or(&sw.name);
                let Some(si) = self.sub_index(es) else { continue };
                let Some(ni) = self.node_index(si, &e.to_state) else { continue };
                let v = self.global((si, ni));
                if !traversable(v) {
                    continue;
                }
                let base = if self.hands_weights && (e.to_state == "shoot" || e.to_state == "melee") { 1000.0 } else { 100.0 };
                let ws = e.weight_scale.map(|w| ((w * 16.0) as i32).clamp(0, 255)).unwrap_or(16) as f32;
                let new = cost[u].saturating_add((base * (ws * 0.0625)) as i32);
                if new < cost[v] {
                    cost[v] = new;
                    prev[v] = u;
                    heap.push(v, new);
                }
            }
        }
        let mut best: Option<usize> = None;
        for &c in &candidates {
            if cost[c] != i32::MAX && best.is_none_or(|b| cost[c] < cost[b]) {
                best = Some(c);
            }
        }
        let mut cur = best?;
        let mut path = Vec::new();
        while cur != start && path.len() < 32 {
            path.push(self.local(cur));
            cur = prev[cur];
            if cur == usize::MAX {
                return None;
            }
        }
        path.reverse();
        Some(path)
    }

    fn sub_or_current(&self, sub: Option<&str>) -> Option<usize> {
        match sub {
            Some(s) => self.sub_index(s),
            None => self.cur_node.map(|k| k.0),
        }
    }

    fn current_node(&self) -> Option<Key> {
        self.cur_node
    }

    /// Model indices a node has trees for.
    fn models_of(&self, key: Key) -> Vec<usize> {
        let mut v: Vec<usize> = self.web.sub_webs[key.0].nodes[key.1].trees.iter().map(|t| t.model_index).collect();
        v.dedup();
        v
    }

    fn root_of(&self, model: usize) -> Option<&Root> {
        self.roots.iter().find(|(m, _)| *m == model).map(|(_, r)| r)
    }

    /// The weapon model index the current sub-web animates.
    pub fn weapon_model(&self) -> Option<usize> {
        let (s, _) = self.cur_node?;
        self.web.sub_webs[s].nodes.iter().flat_map(|n| n.trees.iter()).map(|t| t.model_index).find(|&m| m != 0)
    }

    fn clear_request(&mut self) {
        self.request = None;
        self.path.clear();
        self.forced_parms = None;
    }

    fn reroll_randoms(&mut self) {
        for k in ["_random2", "_random3", "_random4", "_random5", "_random6", "_random7", "_random8"] {
            if self.scalars.contains_key(k) {
                let m: u32 = k[7..].parse().unwrap_or(2);
                self.rng = self.rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
                self.scalars.insert(k.to_string(), ((self.rng >> 16) % m) as f32);
            }
        }
    }

    /// Puts the web straight into a state (initialisation; not an engine request).
    pub fn set_state(&mut self, sub: &str, state: &str) -> bool {
        let Some(s) = self.sub_index(sub) else { return false };
        let Some(n) = self.node_index(s, state) else { return false };
        self.ensure_node((s, n));
        self.start_node((s, n), 0, self.now);
        self.roots = self.models_of((s, n)).into_iter().map(|m| (m, Root::Node((s, n)))).collect();
        self.cur_node = Some((s, n));
        self.clear_request();
        true
    }

    /// ChangeState / ChangeStateVia (0x141700000 / 0x141700ee0) with the caller's interruptPath / interruptBlend.
    /// Requesting the current node clears any pending path; requesting a via node that is current is a plain
    /// ChangeState. The path is built at the next update; an unknown state is ignored (returns false).
    pub fn request(&mut self, sub: Option<&str>, to: &str, via: Option<(Option<&str>, &str)>, interrupt_path: u8, interrupt_blend: u8) -> bool {
        let Some(cur) = self.current_node() else {
            return sub.is_some_and(|s| self.set_state(s, via.map(|v| v.1).unwrap_or(to)));
        };
        let Some(dest_sub) = self.sub_or_current(sub) else { return false };
        let Some(dest_node) = self.node_index(dest_sub, to) else { return false };
        let mut via_req = None;
        if let Some((vsub, vstate)) = via {
            let Some(vs) = self.sub_or_current(vsub) else { return false };
            let Some(vn) = self.node_index(vs, vstate) else { return false };
            if (vs, vn) != cur {
                via_req = Some((vs, vstate.to_string()));
            }
        }
        if via_req.is_none() && (dest_sub, dest_node) == cur {
            self.clear_request();
            return true;
        }
        self.request = Some(Request { dest_sub, dest_state: to.to_string(), via: via_req, forced: None, then: None, interrupt_path, interrupt_blend });
        true
    }

    /// ChangeState with the hands' usual interruptPath 1 / interruptBlend 1.
    pub fn change_state(&mut self, sub: Option<&str>, to: &str) -> bool {
        self.request(sub, to, None, 1, 1)
    }

    /// ChangeStateVia (via in the same sub-web as `to`) with interruptPath 1 / interruptBlend 1.
    pub fn change_state_via(&mut self, sub: Option<&str>, to: &str, via: Option<&str>) -> bool {
        self.request(sub, to, via.map(|v| (sub, v)), 1, 1)
    }

    /// ForceState (0x141705510): a one-edge path to the state with caller blend parms (window and duration
    /// from `parms`); interruptPath 1, interruptBlend 1. Requesting the current node clears pending requests.
    pub fn force_state(&mut self, sub: Option<&str>, state: &str, parms: BlendParms) -> bool {
        let Some(cur) = self.current_node() else {
            return sub.is_some_and(|s| self.set_state(s, state));
        };
        let Some(s) = self.sub_or_current(sub) else { return false };
        let Some(n) = self.node_index(s, state) else { return false };
        if (s, n) == cur {
            self.clear_request();
            return true;
        }
        self.request = Some(Request { dest_sub: s, dest_state: state.to_string(), via: None, forced: Some(((s, n), parms)), then: None, interrupt_path: 1, interrupt_blend: 1 });
        true
    }

    /// Replan (0x14170b7d0): the stored request becomes a path from the current node.
    fn replan(&mut self) {
        let Some(req) = self.request.take() else { return };
        let Some(cur) = self.current_node() else { return };
        self.path.clear();
        self.forced_parms = None;
        self.interrupt_path = req.interrupt_path;
        self.interrupt_blend = req.interrupt_blend;
        if let Some((dest, parms)) = req.forced {
            self.path = vec![dest];
            if let Some((ts, tstate)) = &req.then {
                if let Some(p) = self.find_path(dest, *ts, tstate) {
                    self.path.extend(p);
                }
            }
            self.forced_parms = Some(parms);
            return;
        }
        let mut path = Vec::new();
        let mut from = cur;
        if let Some((vs, vstate)) = &req.via {
            let Some(p) = self.find_path(from, *vs, vstate) else { return };
            if let Some(&last) = p.last() {
                from = last;
            }
            path.extend(p);
        }
        let Some(p) = self.find_path(from, req.dest_sub, &req.dest_state) else { return };
        path.extend(p);
        self.path = path;
    }

    /// StartNode (scalar pairs, then restart CLAMP leaves and weightless REPEAT leaves at `start_frame`),
    /// then re-anchor weightless leaves at `dest_frame`.
    fn start_node(&mut self, key: Key, dest_frame: i32, t: i32) {
        self.reroll_randoms();
        self.apply_pairs(key, t);
        let data = self.data.clone();
        let Some(n) = self.nodes.get_mut(&key) else { return };
        for tr in &mut n.trees {
            let tpl = tr.tpl.clone();
            for (li, leaf) in tr.leaves.iter_mut().enumerate() {
                let Some(cur) = leaf.alias else { continue };
                // The functor walk reaches a leaf once per reference (a mirrored blend-space entry twice: two
                // Restarts, initCounter += 2); leaves outside the equation are not reached.
                let visits = tpl.visits.get(li).copied().unwrap_or(0);
                // idStartLeafPlayingFunctor (0x1417246e0): the first alias entry whose anim is the leaf's current
                // anim decides; Restart (0x1415d4e50) stores that alias's wrap (select leaves too, created REPEAT).
                // The per-frame select never changes wrap afterwards.
                let a = tpl.aliases.iter().position(|al| al.0 == tpl.aliases[cur].0).unwrap_or(cur);
                let (_, alias_wrap, _) = tpl.aliases[a];
                let m = data.meta.get(&tpl.aliases[a].0).copied();
                if leaf.last_total_weight <= 0.0 || alias_wrap == Wrap::Clamp {
                    for _ in 0..visits {
                        leaf.restart(t, 0, alias_wrap, m);
                    }
                }
                if leaf.last_total_weight <= 0.0 && visits > 0 {
                    leaf.anchor(t, dest_frame, m);
                }
            }
        }
    }

    /// BlendSetup (0x14170b500) for an edge into `dest`.
    fn take_edge(&mut self, p: &BlendParms, dest: Key) {
        let t = self.now;
        self.ensure_node(dest);
        // destEndRelative is not used by the player webs' text.
        self.start_node(dest, p.dest_start_frame as i32, t);
        let dur_ms = (p.dest_duration as i32 * 1000) / EDGE_HZ;
        let curve = match p.blend_type.as_deref() {
            Some("BLEND_EASEIN") => Curve::EaseIn,
            Some("BLEND_EASEOUT") => Curve::EaseOut,
            Some("BLEND_EASEIN_EASEOUT") => Curve::EaseInOut,
            _ => Curve::Linear,
        };
        if let Some(src) = self.current_node() {
            self.node_events.push((1, src));
        }
        self.node_events.push((0, dest));
        let models = self.models_of(dest);
        // Crossing sub-webs: the old weapon model state is dropped.
        if self.cur_node.is_some_and(|c| c.0 != dest.0) {
            self.roots.retain(|(m, _)| *m == 0 || models.contains(m));
        }
        for m in models {
            let blend = |left: Root| {
                if dur_ms < 1 {
                    Root::Blend { left: Box::new(left), right: dest, cur: 1.0, rate: 0.0, curve }
                } else {
                    Root::Blend { left: Box::new(left), right: dest, cur: 0.0, rate: 1000.0 / dur_ms as f32, curve }
                }
            };
            match self.roots.iter_mut().find(|(rm, _)| *rm == m) {
                Some((_, r)) => {
                    let left = std::mem::replace(r, Root::Node(dest));
                    *r = blend(left);
                }
                None => self.roots.push((m, Root::Node(dest))),
            }
        }
        self.cur_node = Some(dest);
    }

    /// One frame: the web Update (scalar pairs, BlendProgress, replan, pending edge), then the blend-command build
    /// (alphas, leaf weights) and the event list. `now` is game time.
    pub fn update(&mut self, now: i32) -> Vec<FiredEvent> {
        let prev_time = self.now;
        self.now = now;
        if self.roots.is_empty() {
            return Vec::new();
        }
        let mut live = Vec::new();
        for (_, r) in &self.roots {
            r.keys(&mut live);
        }
        live.sort();
        live.dedup();
        for k in &live {
            self.apply_pairs(*k, now);
        }
        self.node_events.clear();
        // BlendProgress: a finished root branch collapses to its destination (web events from the hands model).
        for (m, r) in self.roots.iter_mut() {
            if let Root::Blend { left, right, cur, rate, .. } = r {
                if *rate == 0.0 || *cur >= 1.0 {
                    let (out, inn) = (left.newest(), *right);
                    *r = Root::Node(inn);
                    if *m == 0 {
                        self.node_events.push((3, out));
                        self.node_events.push((2, inn));
                    }
                }
            }
        }
        let had_edge = !self.path.is_empty();
        if self.request.as_ref().is_some_and(|r| !had_edge || r.interrupt_path > 0) {
            self.replan();
        }
        if let Some(&next) = self.path.first() {
            let mut go = true;
            if self.is_blending() {
                match self.interrupt_blend {
                    0 => go = false,
                    1 => {
                        // Every model root := root.right. INTERIM: the snap reports the dropped source blend-out and
                        // the dest blend-in as done.
                        for (m, r) in self.roots.iter_mut() {
                            if let Root::Blend { left, right, .. } = r {
                                let (out, inn) = (left.newest(), *right);
                                *r = Root::Node(inn);
                                if *m == 0 {
                                    self.node_events.push((3, out));
                                    self.node_events.push((2, inn));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if go {
                let cur = self.current_node().unwrap();
                let parms = match self.forced_parms.clone() {
                    Some(p) => Some(p),
                    None => self.edge_between(cur, next).map(|e| e.parms.clone()),
                };
                match parms {
                    None => self.path.clear(),
                    Some(parms) => {
                        let take = if self.interrupt_path == 2 {
                            self.interrupt_path = 1;
                            true
                        } else {
                            self.check_window(cur, &parms) == Window::Take
                        };
                        if take {
                            self.forced_parms = None;
                            self.take_edge(&parms, next);
                            self.path.remove(0);
                            // Pass-through nodes (customFlags AW_CF_PASSTHROUGH, vslot 40) are skipped along the
                            // path: their outgoing edge is taken on the same frame. INTERIM: without its window.
                            while let (Some(cur), Some(&n2)) = (self.current_node(), self.path.first()) {
                                if !self.web.sub_webs[cur.0].nodes[cur.1].custom_flags.iter().any(|f| f == "AW_CF_PASSTHROUGH") {
                                    break;
                                }
                                let Some(p2) = self.edge_between(cur, n2).map(|e| e.parms.clone()) else { break };
                                self.take_edge(&p2, n2);
                                self.path.remove(0);
                            }
                        }
                    }
                }
            }
        }
        // Build: branch alphas (UpdateAlpha, dt capped at 64 ticks), then leaf weights.
        let dt = (now - self.last_build).clamp(0, MAX_BUILD_DT);
        self.last_build = now;
        if dt > 0 {
            fn advance(r: &mut Root, step: f32) {
                if let Root::Blend { left, cur, rate, .. } = r {
                    advance(left, step);
                    let target = 1.0f32;
                    let d = target - *cur;
                    if d > 0.0 {
                        let c = *cur + step * *rate;
                        *cur = if c > target - 1e-6 { target } else { c };
                    } else if d < 0.0 {
                        let c = *cur - step * *rate;
                        *cur = if c < target + 1e-6 { target } else { c };
                    }
                }
            }
            for (_, r) in self.roots.iter_mut() {
                advance(r, dt as f32 / TICKS as f32);
            }
        }
        let roots = self.roots.clone();
        for (m, r) in &roots {
            self.weigh(r, 1.0, *m);
        }
        let fired = self.events(&roots, prev_time, now);
        self.last_fired = fired.clone();
        fired
    }

    /// Web events of the last update as (kind, sub-web, state).
    pub fn node_events(&self) -> Vec<(u8, &str, &str)> {
        self.node_events
            .iter()
            .map(|&(k, (s, n))| {
                let sw = &self.web.sub_webs[s];
                (k, sw.name.as_str(), sw.nodes[n].state.as_str())
            })
            .collect()
    }

    /// GetNodeAnimLength (0x141706640 -> 0x141706790) as the hands call it: numFrames (the full N) of alias
    /// entry `alias_index` of the state's node for `model_index` (the idHands callers pass model 0, alias 0).
    /// Leaves, scalars, alphas and the current node play no part.
    pub fn node_anim_frames(&self, model_index: usize, sub: &str, state: &str, alias_index: usize) -> Option<(i32, i32)> {
        let s = self.sub_index(sub)?;
        let n = self.node_index(s, state)?;
        let tree = self.web.sub_webs[s].nodes[n].trees.iter().find(|t| t.model_index == model_index)?;
        let alias = tree.anims.get(alias_index)?;
        let m = self.meta(&alias.name.to_ascii_lowercase())?;
        Some((m.num_frames as i32, m.frame_rate as i32))
    }

    /// numFrames of the hands anim a state plays, as the idHands bring/shoot rate code asks for it.
    pub fn state_frames(&self, sub: &str, state: &str) -> Option<i32> {
        self.node_anim_frames(0, sub, state, 0).map(|(n, _)| n)
    }

    /// numFrames / frameRate of the anim an md6Def alias of `model` resolves to.
    pub fn alias_meta(&self, model: &str, alias: &str) -> Option<AnimMeta> {
        let anim = self.data.aliases.get(&model.to_ascii_lowercase())?.get(alias)?;
        self.meta(anim)
    }

    /// Stores each leaf's weight (lastTotalWeight) for the current build.
    fn weigh(&mut self, r: &Root, w: f32, model: usize) {
        match r {
            Root::Node(k) => {
                let scalars = &self.scalars;
                let Some(n) = self.nodes.get_mut(k) else { return };
                for tr in &mut n.trees {
                    let tpl = tr.tpl.clone();
                    if tpl.nodes.is_empty() || tpl.model_index != model {
                        continue;
                    }
                    weigh_tree(tr, tpl.root, w, &|s| scalars.get(s).copied().unwrap_or(0.0));
                }
            }
            Root::Blend { left, right, cur, curve, .. } => {
                let a = ease(*curve, *cur, 1.0);
                self.weigh(left, w * (1.0 - a), model);
                self.weigh(&Root::Node(*right), w * a, model);
            }
        }
    }

    /// Leaves of the live tree in DFS order (left first) with their minor-side flag:
    /// (node, tree, leaf, minor).
    fn collect_leaves(&self, r: &Root, model: usize, minor: bool, out: &mut Vec<(Key, usize, usize, bool)>) {
        match r {
            Root::Node(k) => {
                let Some(n) = self.nodes.get(k) else { return };
                for (ti, tr) in n.trees.iter().enumerate() {
                    if tr.tpl.nodes.is_empty() || tr.tpl.model_index != model {
                        continue;
                    }
                    self.collect_tree(*k, ti, tr.tpl.root, minor, out);
                }
            }
            Root::Blend { left, right, cur, .. } => {
                let (lm, rm) = self.minor_sides(*cur, |rt, _| rt.root_playing(left, model), |rt, _| rt.root_playing(&Root::Node(*right), model));
                self.collect_leaves(left, model, minor || lm, out);
                self.collect_leaves(&Root::Node(*right), model, minor || rm, out);
            }
        }
    }

    /// Leaf collection 0x1416e8080 walks the current children (a blend space's bracket, a blendA's base and
    /// delta; the same leaf on both sides is collected twice); ops 3..6 set no minor side.
    fn collect_tree(&self, key: Key, ti: usize, x: usize, minor: bool, out: &mut Vec<(Key, usize, usize, bool)>) {
        let tr = &self.nodes[&key].trees[ti];
        if let TNode::Leaf(l) = &tr.tpl.nodes[x] {
            out.push((key, ti, *l, minor));
            return;
        }
        let Some((op, a, b, c)) = tr.kids(x, &|s| self.scalar(s)) else { return };
        let side = |n: Option<usize>| n.is_some_and(|n| self.tree_playing(key, ti, n));
        let (lm, rm) = if matches!(op, BlendOp::AddLeft | BlendOp::AddRight | BlendOp::SubLeft | BlendOp::SubRight) {
            (false, false)
        } else {
            self.minor_sides(c, |_, _| side(a), |_, _| side(b))
        };
        if let Some(a) = a {
            self.collect_tree(key, ti, a, minor || lm, out);
        }
        if let Some(b) = b {
            self.collect_tree(key, ti, b, minor || rm, out);
        }
    }

    /// Minor side of a LERP branch: alpha <= 0.5 -> right is minor if the left has a playing leaf; else the left
    /// is minor if the right has one.
    fn minor_sides(&self, c: f32, left_playing: impl Fn(&Self, ()) -> bool, right_playing: impl Fn(&Self, ()) -> bool) -> (bool, bool) {
        if c <= 0.5 {
            (false, left_playing(self, ()))
        } else {
            (right_playing(self, ()), false)
        }
    }

    fn root_playing(&self, r: &Root, model: usize) -> bool {
        let mut keys = Vec::new();
        r.keys(&mut keys);
        keys.iter().any(|k| {
            self.nodes.get(k).is_some_and(|n| (0..n.trees.len()).any(|ti| n.trees[ti].tpl.model_index == model && !n.trees[ti].tpl.nodes.is_empty() && self.tree_playing(*k, ti, n.trees[ti].tpl.root)))
        })
    }

    fn tree_playing(&self, key: Key, ti: usize, x: usize) -> bool {
        let tr = &self.nodes[&key].trees[ti];
        match &tr.tpl.nodes[x] {
            TNode::Leaf(l) => {
                let leaf = &tr.leaves[*l];
                leaf.alias.and_then(|a| self.meta(&tr.tpl.aliases[a].0)).is_some_and(|m| leaf.is_playing(self.now, m))
            }
            _ => tr.kids(x, &|s| self.scalar(s)).is_some_and(|(_, a, b, _)| a.is_some_and(|a| self.tree_playing(key, ti, a)) || b.is_some_and(|b| self.tree_playing(key, ti, b))),
        }
    }

    /// BuildAnimEventList for every model.
    fn events(&mut self, roots: &[(usize, Root)], prev_time: i32, now: i32) -> Vec<FiredEvent> {
        let mut leaves = Vec::new();
        for (m, r) in roots {
            self.collect_leaves(r, *m, false, &mut leaves);
        }
        let data = self.data.clone();
        let mut fired = Vec::new();
        // Untrack events whose leaf is gone or whose playhead moved back before them.
        let mut alive: Vec<(usize, String, i64, u8, i64)> = Vec::new();
        for &(key, ti, li, _) in &leaves {
            let tr = &self.nodes[&key].trees[ti];
            let leaf = &tr.leaves[li];
            let Some(a) = leaf.alias else { continue };
            let name = tr.tpl.aliases[a].0.clone();
            let Some(m) = data.meta.get(&name).copied() else { continue };
            alive.push((tr.tpl.model_index, name, leaf.loop_count(now, m), leaf.init_counter, leaf.int_frame(now, m)));
        }
        self.tracked.retain(|t| alive.iter().any(|(mi, an, lc, ic, f)| *mi == t.model_index && *an == t.anim && *lc == t.loop_count && *ic == t.init && *f >= t.frame));
        for &(key, ti, li, minor) in &leaves {
            let model_index;
            let name;
            let (from, to, loop_count, init);
            {
                let tr = self.nodes.get_mut(&key).unwrap().trees.get_mut(ti).unwrap();
                model_index = tr.tpl.model_index;
                let Some(a) = tr.leaves[li].alias else { continue };
                name = tr.tpl.aliases[a].0.clone();
                let Some(m) = data.meta.get(&name).copied() else { continue };
                let leaf = &mut tr.leaves[li];
                let cur = leaf.int_frame(now, m);
                let mut f = leaf.int_frame(prev_time, m);
                if leaf.last_event_frame >= 0 && leaf.last_event_frame != f && leaf.last_event_frame <= cur {
                    f = leaf.last_event_frame;
                }
                leaf.last_event_frame = cur;
                from = f;
                to = cur;
                loop_count = leaf.loop_count(now, m);
                init = leaf.init_counter;
            }
            let Some(evs) = data.events.get(&model_index).and_then(|e| e.get(&name)) else { continue };
            for (ei, e) in evs.iter().enumerate() {
                let fr = e.frame as i64;
                let hit = if from <= to { from <= fr && fr <= to } else { fr >= from || fr <= to };
                if !hit || (minor && CANSKIP.contains(&e.name.as_str())) {
                    continue;
                }
                let t = Tracked { model_index, anim: name.clone(), event: ei, frame: fr, loop_count, init };
                if self.tracked.contains(&t) {
                    continue;
                }
                self.tracked.push(t);
                fired.push(FiredEvent { model_index, anim: name.clone(), event: e.clone() });
            }
        }
        fired
    }

    /// The pose to evaluate for the hands (`weapon == false`, modelIndex 0) or the current sub-web's weapon model.
    pub fn pose_tree(&self, weapon: bool) -> Option<PoseNode> {
        let model = if weapon { self.weapon_model()? } else { 0 };
        self.pose_tree_model(model)
    }

    /// The pose of one model's anim state.
    /// The live tree's leaves of one model in event-collection order (0x1416e8080: a blend space's bracket, a
    /// blendA's base and delta) with their anim and lastTotalWeight from the last build.
    pub fn leaf_weights(&self, model: usize) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        if let Some(r) = self.root_of(model) {
            self.collect_leaves(r, model, false, &mut out);
        }
        out.iter()
            .filter_map(|&(k, ti, li, _)| {
                let tr = &self.nodes[&k].trees[ti];
                let leaf = &tr.leaves[li];
                leaf.alias.map(|a| (tr.tpl.aliases[a].0.clone(), leaf.last_total_weight))
            })
            .collect()
    }

    pub fn pose_tree_model(&self, model: usize) -> Option<PoseNode> {
        self.root_pose(self.root_of(model)?, model)
    }

    fn root_pose(&self, r: &Root, model: usize) -> Option<PoseNode> {
        match r {
            Root::Node(k) => self.node_pose(*k, model),
            Root::Blend { left, right, cur, curve, .. } => {
                let rp = if *cur == 0.0 { None } else { self.node_pose(*right, model) };
                let lp = if *cur == 1.0 && rp.is_some() { None } else { self.root_pose(left, model) };
                match (lp, rp) {
                    (Some(l), Some(r)) => Some(PoseNode::Lerp { left: Box::new(l), right: Box::new(r), alpha: ease(*curve, *cur, 1.0) }),
                    (l, r) => l.or(r),
                }
            }
        }
    }

    fn node_pose(&self, key: Key, model: usize) -> Option<PoseNode> {
        let n = self.nodes.get(&key)?;
        let ti = n.trees.iter().position(|t| t.tpl.model_index == model)?;
        let tr = &n.trees[ti];
        if tr.tpl.nodes.is_empty() {
            return Some(PoseNode::Bind);
        }
        Some(self.expr_pose(tr, tr.tpl.root).unwrap_or(PoseNode::Bind))
    }

    /// The blend commands of a subtree as a pose (None: it emits no commands).
    fn expr_pose(&self, tr: &TreeInst, x: usize) -> Option<PoseNode> {
        if let TNode::Leaf(l) = &tr.tpl.nodes[x] {
            let leaf = &tr.leaves[*l];
            let Some(a) = leaf.alias else { return Some(PoseNode::Bind) };
            let name = &tr.tpl.aliases[a].0;
            return Some(match self.meta(name) {
                Some(m) => {
                    let (frame, frac) = leaf.sample_pos(self.now, m);
                    PoseNode::Leaf { anim: name.clone(), frame, frac }
                }
                None => PoseNode::Bind,
            });
        }
        let (op, a, b, c) = tr.kids(x, &|s| self.scalar(s))?;
        let (le, re) = op_emits(op, c);
        let l = if le { a.and_then(|a| self.expr_pose(tr, a)) } else { None };
        let r = if re { b.and_then(|b| self.expr_pose(tr, b)) } else { None };
        match (l, r) {
            (Some(l), Some(r)) => Some(match op {
                BlendOp::Lerp | BlendOp::RefLerp | BlendOp::Blend => PoseNode::Lerp { left: Box::new(l), right: Box::new(r), alpha: c },
                _ => PoseNode::Op { op, left: Box::new(l), right: Box::new(r), alpha: c },
            }),
            (l, r) => l.or(r),
        }
    }

    /// The newest node's timing leaf (anim, int frame) for traces.
    pub fn playhead(&self) -> Option<(String, i64)> {
        let k = self.current_node()?;
        let (ti, li) = self.timing_leaf(k, "", "", false)?;
        let tr = &self.nodes[&k].trees[ti];
        let leaf = &tr.leaves[li];
        let name = leaf.alias.map(|a| tr.tpl.aliases[a].0.clone())?;
        let m = self.meta(&name)?;
        Some((name, leaf.int_frame(self.now, m)))
    }

    /// True when the newest node's timing leaf is clamped and has stopped playing.
    pub fn finished(&self) -> bool {
        let Some(k) = self.current_node() else { return true };
        let Some((ti, li)) = self.timing_leaf(k, "", "", false) else { return true };
        let tr = &self.nodes[&k].trees[ti];
        let leaf = &tr.leaves[li];
        leaf.alias.and_then(|a| self.meta(&tr.tpl.aliases[a].0)).is_none_or(|m| !leaf.is_playing(self.now, m))
    }
}

/// The hands driver (weapons::hands) runs the web through this trait.
impl crate::weapons::hands::HandsWeb for AnimWebRuntime {
    fn current(&self) -> Option<(&str, &str)> {
        AnimWebRuntime::current(self)
    }
    fn is_blending(&self) -> bool {
        AnimWebRuntime::is_blending(self)
    }
    fn set_scalar(&mut self, name: &str, value: f32) {
        AnimWebRuntime::set_scalar(self, name, value)
    }
    fn change_state(&mut self, sub: &str, to: &str) {
        self.request(Some(sub), to, None, 1, 1);
    }
    fn change_state_via(&mut self, sub: &str, to: &str, via: &str) {
        self.request(Some(sub), to, Some((Some(sub), via)), 1, 1);
    }
    fn force_state(&mut self, sub: &str, to: &str, blend_frames: i32) {
        let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: blend_frames as f32, ..Default::default() };
        AnimWebRuntime::force_state(self, Some(sub), to, parms);
    }
    fn force_state_via(&mut self, sub: &str, to: &str, via: &str, blend_frames: i32) {
        // INTERIM (0x1417046b0 not decoded): a forced edge into `to`, then the planned path on to `via`.
        let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: blend_frames as f32, ..Default::default() };
        if AnimWebRuntime::force_state(self, Some(sub), to, parms) {
            if let (Some(r), Some(vs)) = (self.request.as_mut(), self.web.sub_webs.iter().position(|x| x.name == sub)) {
                r.then = Some((vs, via.to_string()));
            }
        }
    }
    fn alias_anim(&self, model: &str, alias: &str) -> Option<(i32, i32)> {
        self.alias_meta(model, alias).map(|m| (m.num_frames as i32, m.frame_rate as i32))
    }
    fn state_anim_frames(&self, sub: &str, state: &str) -> Option<i32> {
        self.state_frames(sub, state)
    }
    fn update(&mut self, now_ms: i32) -> Vec<crate::weapons::hands::WebEvent> {
        use crate::weapons::hands::WebEvent;
        let fired = AnimWebRuntime::update(self, now_ms);
        let mut out: Vec<WebEvent> = self.node_events().into_iter().map(|(kind, sub, state)| WebEvent::Node { kind, sub: sub.to_string(), state: state.to_string() }).collect();
        out.extend(fired.into_iter().map(|f| WebEvent::Anim { int: f.event.param("int").and_then(|a| a.f32()).map(|v| v as i32), name: f.event.name }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(rate: f32, wrap: Wrap) -> Leaf {
        Leaf { alias: Some(0), wrap, rate, start: 960, last_event_frame: -1, init_counter: 0, last_total_weight: 0.0, best_bits: None }
    }

    #[test]
    fn leaf_clock_rules() {
        let m = AnimMeta { num_frames: 10, frame_rate: 30 };
        let l = leaf(1.0, Wrap::Clamp);
        // 96 ticks = 3 frames at 30 fps (960 ticks / s).
        assert_eq!(l.int_frame(960 + 96, m), 3);
        assert_eq!(l.int_frame(960 + 5000, m), 9);
        let r = leaf(1.0, Wrap::Repeat);
        // Repeat wraps modulo N-1.
        assert_eq!(r.int_frame(960 + 288, m), 0);
        assert_eq!(r.sample_pos(960 + 288, m), (0, 0.0));
        assert_eq!(r.sample_pos(960 + 272, m).0, 8);
        let mut s = leaf(1.0, Wrap::Clamp);
        let before = s.int_frame(1056, m);
        s.set_rate(1056, 2.0);
        assert_eq!(s.int_frame(1056, m), before);
        assert_eq!(s.int_frame(1056 + 48, m), 6);
    }

    #[test]
    fn ease_curves() {
        for c in [Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::EaseInOut] {
            assert_eq!(ease(c, 0.0, 1.0), 0.0);
            assert_eq!(ease(c, 1.0, 1.0), 1.0);
        }
        assert_eq!(ease(Curve::EaseIn, 0.5, 1.0), 0.25);
        assert_eq!(ease(Curve::EaseOut, 0.5, 1.0), 0.75);
        assert_eq!(ease(Curve::EaseInOut, 0.25, 1.0), 0.125);
        assert_eq!(ease(Curve::EaseInOut, 0.75, 1.0), 0.875);
    }

    #[test]
    fn rate_pairs_replace_and_select() {
        let web_src = "{\n props {\n modelInfos {\n \"player/fp_hands.md6\" \"\"\n }\n }\n scalars {\n sel 0\n r 1\n }\n subWebs {\n subWeb \"w\" {\n node \"a\" {\n blendTrees {\n tree {\n modelIndex 0\n blendEq \"anim[sel+1]*r\"\n anims {\n alias {\n name \"x0.md6anim\"\n wrap WRAP_CLAMP\n }\n alias {\n name \"x1.md6anim\"\n wrap WRAP_REPEAT\n rate 0.25\n }\n }\n }\n }\n }\n }\n }\n}\n";
        let web = Arc::new(AnimWeb::parse(web_src).unwrap());
        let mut data = AnimData::default();
        data.meta.insert("x0.md6anim".into(), AnimMeta { num_frames: 10, frame_rate: 30 });
        data.meta.insert("x1.md6anim".into(), AnimMeta { num_frames: 10, frame_rate: 30 });
        let mut rt = AnimWebRuntime::new(web, Arc::new(data));
        rt.set_scalar("r", 2.0);
        assert!(rt.set_state("w", "a"));
        rt.update(96);
        let Some(PoseNode::Leaf { anim, frame, .. }) = rt.pose_tree(false) else { panic!() };
        assert_eq!(anim, "x1.md6anim");
        // Rate 2 replaces the alias rate 0.25: 96 ticks -> 6 frames.
        assert_eq!(frame, 6);
    }

    /// The Possessed's death node (Best over Filter chains) from the user's install (skipped without one).
    #[test]
    fn best_filter_zombie_death() {
        let Some(doom) = idres::find_install() else { return };
        let c = Container::open(&doom.join("base"), "gameresources").unwrap();
        let src = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/zion/characters/monsters/zombie.decl").unwrap()).into_owned();
        let web = Arc::new(AnimWeb::parse(&src).unwrap());
        let data = Arc::new(AnimData::load(&c, &web, &["death", "hands_combat"]));
        let pick = |tags: &[&str]| {
            let mut rt = AnimWebRuntime::new(web.clone(), data.clone());
            rt.hands_weights = false;
            assert!(rt.set_state("hands_combat", "idle"));
            rt.update(10);
            for t in tags {
                rt.set_scalar(t, 1.0);
            }
            let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: 5.0, ..Default::default() };
            assert!(rt.force_state(Some("death"), "death", parms));
            for k in 1..40 {
                rt.update(10 + k * 16);
            }
            match rt.pose_tree_model(0) {
                Some(PoseNode::Leaf { anim, .. }) => anim,
                other => panic!("{other:?}"),
            }
        };
        // Head hit: the head filter leaves the two headshot deaths (the source filter has no survivor and undoes).
        assert!(pick(&["heavy", "front", "head", "not_injured"]).contains("/death/headshot_v"));
        // Light chest hit from the front, standing: stationary/light/front/midsection (the only light front chest).
        assert_eq!(pick(&["light", "front", "chest", "not_injured"]), "md6/characters/monsters/zombie/base/motion/death/stationary/light/front/midsection.md6anim");
        // Left leg from behind: the back filter leaves the back upper-body deaths and the headshots; none has
        // left_leg, so the part filter retries with the DEFAULT part bit (head) -> the headshot deaths.
        let a = pick(&["light", "back", "left_leg", "not_injured"]);
        assert!(a.contains("/death/headshot_v"), "{a}");

        // Falter: forced into falter/hands_front, then idle is requested; the path runs through the pass-through
        // falter/exit (no tree) at hands_front's end window and lands in hands_combat/idle.
        let data = Arc::new(AnimData::load(&c, &web, &["falter", "hands_combat"]));
        let mut rt = AnimWebRuntime::new(web.clone(), data);
        rt.hands_weights = false;
        assert!(rt.set_state("hands_combat", "idle"));
        rt.update(10);
        let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: 2.0, ..Default::default() };
        assert!(rt.force_state(Some("falter"), "hands_front", parms));
        rt.update(26);
        assert_eq!(rt.current(), Some(("falter", "hands_front")));
        assert!(rt.change_state(Some("hands_combat"), "idle"));
        let mut t = 26;
        while rt.current() != Some(("hands_combat", "idle")) && t < 26 + 960 * 10 {
            t += 16;
            rt.update(t);
            assert_ne!(rt.current(), Some(("falter", "exit")), "stuck in the pass-through node");
        }
        assert_eq!(rt.current(), Some(("hands_combat", "idle")));
    }

    /// Install-backed: an SP web's runtime on `sub`/`node` after two updates with `scalars` set.
    fn monster(decl: &str, sub: &str, node: &str, scalars: &[(&str, f32)]) -> Option<AnimWebRuntime> {
        let doom = idres::find_install()?;
        let c = Container::open(&doom.join("base"), "gameresources").unwrap();
        let src = String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/animweb/zion/characters/monsters/{decl}.decl")).unwrap()).into_owned();
        let web = Arc::new(AnimWeb::parse(&src).unwrap());
        let data = Arc::new(AnimData::load(&c, &web, &[sub]));
        let mut rt = AnimWebRuntime::new(web, data);
        rt.hands_weights = false;
        for (k, v) in scalars {
            rt.set_scalar(k, *v);
        }
        assert!(rt.set_state(sub, node));
        rt.update(10);
        rt.update(26);
        Some(rt)
    }

    fn short(a: &str) -> &str {
        a.rsplit('/').next().unwrap().trim_end_matches(".md6anim")
    }

    fn weights(rt: &AnimWebRuntime) -> Vec<(String, f32)> {
        rt.leaf_weights(0).into_iter().map(|(a, w)| (short(&a).to_string(), w)).collect()
    }

    #[test]
    fn sort_space_mirror() {
        // Zombie walk variant 0 coordinates; the exe's 0x1415d39f0 gives this order (tools/animweb_space_oracle.py).
        let cs = [0.0, 45.0, -45.0, 90.0, 55.0, 130.0, -90.0, -55.0, -130.0, 180.0, 140.0, -140.0];
        let e: Vec<(usize, f32)> = cs.iter().enumerate().map(|(i, c)| (i, *c)).collect();
        let s: Vec<usize> = sort_space(&e, true).iter().map(|x| x.0).collect();
        assert_eq!(s, [9, 11, 8, 6, 7, 2, 0, 1, 4, 3, 5, 10, 9]);
        assert_eq!(sort_space(&e, false).len(), 12);
        // Equal coordinates: the later entry goes first (lower-bound insert).
        assert_eq!(sort_space(&[(0, 1.0), (1, 1.0)], false).iter().map(|x| x.0).collect::<Vec<_>>(), [1, 0]);
    }

    #[test]
    fn blend_space_zombie_walk() {
        // hands_combat/walk: lerp(blendy1(bodyMoveAngle, filter(previousRandomTags)), blendy1(bodyMoveAngle,
        // filter(randomTags)), tagBlend). With tagBlend 1 the left space keeps weight 0; the right one brackets
        // the angle in variant 0 (no random_k set: the filter falls back to the default bit random_0). Expected
        // brackets / alphas are the exe's (tools/animweb_space_oracle.py on the same sorted list).
        let run = |angle: f32| monster("zombie", "hands_combat", "walk", &[("bodyMoveAngle", angle), ("tagBlend", 1.0)]);
        let Some(rt) = run(30.0) else { return };
        let w = weights(&rt);
        // Left space first (both its bracket leaves at weight 0), then the right space's lo, hi.
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w[0].1 == 0.0 && w[1].1 == 0.0, "{w:?}");
        let right = |rt: &AnimWebRuntime| {
            let w = weights(rt);
            (w[2].0.clone(), w[2].1, w[3].0.clone(), w[3].1)
        };
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        let (lo, wl, hi, wh) = right(&rt);
        assert_eq!((lo.as_str(), hi.as_str()), ("walk_forward", "walk_forward_left_45"));
        assert!(close(wh, 0.6666666865348816) && close(wl, 1.0 - 0.6666666865348816), "{wl} {wh}");
        match rt.pose_tree_model(0) {
            Some(PoseNode::Lerp { left, right, alpha }) => {
                assert!(matches!(*left, PoseNode::Leaf { ref anim, .. } if short(anim) == "walk_forward"));
                assert!(matches!(*right, PoseNode::Leaf { ref anim, .. } if short(anim) == "walk_forward_left_45"));
                assert!(close(alpha, 0.6666666865348816));
            }
            p => panic!("{p:?}"),
        }
        // angle -> (lo, hi, alpha) from the exe, after Init's resolve at 0 left the bracket (-45, 0) cached: -45 is
        // inside it, so it stays (alpha 0) instead of the fresh (-55, -45, 1); same pose.
        for (angle, lo_w, hi_w, a) in [
            (0.0, "walk_forward_right_45", "walk_forward", 1.0),
            (45.0, "walk_forward", "walk_forward_left_45", 1.0),
            (-45.0, "walk_forward_right_45", "walk_forward", 0.0),
            (180.0, "walk_backward_left_45", "walk_backward", 1.0),
            (-180.0, "walk_backward", "walk_backward_right_45", 0.0),
            (100.0, "walk_left", "walk_left_backward_45", 0.25),
            (-100.0, "walk_right_backward_45", "walk_right", 0.75),
        ] {
            let rt = run(angle).unwrap();
            let (lo, wl, hi, wh) = right(&rt);
            assert_eq!((lo.as_str(), hi.as_str()), (lo_w, hi_w), "angle {angle}");
            assert!(close(wh, a) && close(wl, 1.0 - a), "angle {angle}: {wl} {wh}");
            // alpha 1 / 0: one leaf left in the pose (pruned on the raw alpha).
            if a == 1.0 || a == 0.0 {
                let want = if a == 1.0 { hi_w } else { lo_w };
                assert!(matches!(rt.pose_tree_model(0), Some(PoseNode::Leaf { ref anim, .. }) if short(anim) == want), "angle {angle}");
            }
        }
        // A random variant: random_3 picks the d walk set; previous variant random_1 on the left at tagBlend 0.5.
        let rt = monster("zombie", "hands_combat", "walk", &[("bodyMoveAngle", 30.0), ("tagBlend", 0.5), ("random_3", 1.0), ("p/random_1", 1.0)]).unwrap();
        let w = weights(&rt);
        assert_eq!(w.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["walk_forward_b", "walk_forward_b_left_45", "walk_forward_d", "walk_forward_d_left_45"]);
        let a = 0.6666666865348816f32;
        for (i, want) in [(1.0 - a) * 0.5, a * 0.5, (1.0 - a) * 0.5, a * 0.5].into_iter().enumerate() {
            assert!(close(w[i].1, want), "{w:?}");
        }
        // The bracket follows the angle frame by frame (the cached bracket is left when the angle leaves it).
        let mut rt = run(30.0).unwrap();
        rt.set_scalar("bodyMoveAngle", 50.0);
        rt.update(42);
        assert_eq!(right(&rt).0, "walk_forward_left_45");
        assert_eq!(right(&rt).2, "walk_left_forward_45");
    }

    #[test]
    fn blend_space_events_fire() {
        // Footsteps (ae_leftFoot / ae_rightFoot) come from the bracketing leaves while walking.
        let Some(mut rt) = monster("zombie", "hands_combat", "walk", &[("bodyMoveAngle", 20.0), ("tagBlend", 1.0)]) else { return };
        let mut anims = Vec::new();
        for k in 0..240 {
            for e in rt.update(26 + k * 16) {
                anims.push(short(&e.anim).to_string());
            }
        }
        assert!(anims.iter().any(|a| a == "walk_forward_left_45"), "{anims:?}");
    }

    #[test]
    fn blenda_aim_hellified_soldier() {
        // rifle_combat/walk_aim: blenda(bodyAimPitch, blend1(bodyAimYaw, anim[0,4]), blend1(bodyAimYaw,
        // anim[5,9]), -70, blend1(bodyAimYaw, anim[10,14]), 70). Pitch -35 -> the 70down delta at alpha 0.5
        // (0x1415d1d20, oracle: delta 0 alpha 0.5); yaw 45 -> forward / 90_left at 0.5 in both spaces.
        let Some(rt) = monster("hellified_soldier", "rifle_combat", "walk_aim", &[("bodyAimYaw", 45.0), ("bodyAimPitch", -35.0)]) else { return };
        let w = weights(&rt);
        let names: Vec<&str> = w.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "walkforward_aiming_forward_stage1",
                "walkforward_aiming_90_left_stage1",
                "walkforward_aiming_forward_stage1_70down",
                "walkforward_aiming_90_left_stage1_70down"
            ]
        );
        // BLENDA weights like LERP: base (1 - a) * w, delta a * w.
        for (i, want) in [0.25f32, 0.25, 0.25, 0.25].into_iter().enumerate() {
            assert!((w[i].1 - want).abs() < 1e-6, "{w:?}");
        }
        match rt.pose_tree_model(0) {
            Some(PoseNode::Op { op: BlendOp::BlendA, left, right, alpha }) => {
                assert_eq!(alpha, 0.5);
                assert!(matches!(*left, PoseNode::Lerp { alpha, .. } if alpha == 0.5));
                assert!(matches!(*right, PoseNode::Lerp { alpha, .. } if alpha == 0.5));
            }
            p => panic!("{p:?}"),
        }
        // Pitch 0: alpha -0.0 -> the delta emits no commands (base only).
        let rt = monster("hellified_soldier", "rifle_combat", "walk_aim", &[("bodyAimYaw", 45.0), ("bodyAimPitch", 0.0)]).unwrap();
        assert!(matches!(rt.pose_tree_model(0), Some(PoseNode::Lerp { alpha, .. }) if alpha == 0.5));
    }

    #[test]
    fn add_right_hellknight_idle() {
        // hands_combat/idle: addR(anim0, blend1(additiveFocusAngle, anim[1,5]), additiveFocusScale). ADD_RIGHT
        // weights: base 1, delta 0 (0x141a38ce0); the pose adds the delta space onto anim0 with the scale.
        let Some(rt) = monster("hellknight", "hands_combat", "idle", &[("additiveFocusAngle", 45.0), ("additiveFocusScale", 0.75)]) else { return };
        let w = weights(&rt);
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!((w[0].1, w[1].1, w[2].1), (1.0, 0.0, 0.0), "{w:?}");
        match rt.pose_tree_model(0) {
            Some(PoseNode::Op { op: BlendOp::AddRight, left, right, alpha }) => {
                assert_eq!(alpha, 0.75);
                assert!(matches!(*left, PoseNode::Leaf { .. }));
                assert!(matches!(*right, PoseNode::Lerp { alpha, .. } if alpha == 0.5));
            }
            p => panic!("{p:?}"),
        }
        // Scale 0: the delta is pruned.
        let rt = monster("hellknight", "hands_combat", "idle", &[("additiveFocusAngle", 45.0), ("additiveFocusScale", 0.0)]).unwrap();
        assert!(matches!(rt.pose_tree_model(0), Some(PoseNode::Leaf { .. })));
    }
}
