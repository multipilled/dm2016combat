//! A running SWF: object heap, sprite/edit-text instances, timelines, tweens and the native script API that
//! DOOM's GUIs use (`idSWFScriptObject_SpriteInstancePrototype` / `_TextInstancePrototype` in the exe).
//!
//! Frames are 1-based as in the engine (`RunTo(1)` is the first frame, 0 means "not started yet").

use std::collections::HashMap;
use std::sync::Arc;

use crate::bswf::{DictEntry, Matrix, Sprite, Swf};
use crate::tags::{self, ColorXform, PlaceObject, Tag};

pub type ObjId = u32;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Arc<str>),
    Obj(ObjId),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Arc::from(s))
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    pub fn as_obj(&self) -> Option<ObjId> {
        if let Value::Obj(o) = self { Some(*o) } else { None }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Num(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Num(v as f64)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Num(v as f64)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::str(v)
    }
}

/// An ActionScript function defined in the SWF.
#[derive(Debug)]
pub struct AsFunction {
    pub code: Arc<[u8]>,
    pub body: std::ops::Range<usize>,
    pub params: Vec<(u8, Arc<str>)>,
    pub register_count: u8,
    pub flags: u16,
    pub v2: bool,
    pub pool: Arc<Vec<Arc<str>>>,
    pub scope: Vec<ObjId>,
    pub target: ObjId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Native {
    // sprite prototype
    GotoAndStop,
    GotoAndPlay,
    Play,
    Stop,
    NextFrame,
    PrevFrame,
    SwfWait,
    SwfTriggeredPause,
    SwapDepths,
    DuplicateMovieClip,
    RemoveMovieClip,
    Tween,
    RemoveTweens,
    ToString,
    // edit text prototype
    CalcNumLines,
    // array prototype
    ArrayPush,
    ArrayPop,
    ArrayJoin,
    ArraySplice,
    ArrayShift,
    ArrayUnshift,
    // _global (idSWF constructor, 0x141615e40)
    PlaySound,
    StopSounds,
    PrecacheSound,
    PrecacheFontFxMaterial,
    GetPlatform,
    GetTruePlatform,
    GetLocalString,
    GetCVarInteger,
    SetCVarInteger,
    StrReplace,
    IsJapanese,
    Acos,
    Cos,
    Sin,
    Round,
    Pow,
    Sqrt,
    Abs,
    Rand,
    Floor,
    Ceil,
    ToUpper,
    Noop,
    // constructors
    ArrayCtor,
    ObjectCtor,
}

#[derive(Debug, Clone)]
pub struct Display {
    pub depth: u16,
    pub character: u16,
    /// Sprite or edit-text instance placed at this depth.
    pub inst: Option<ObjId>,
    pub matrix: Matrix,
    pub cxform: ColorXform,
    pub ratio: f32,
    pub clip_depth: u16,
    pub blend: u8,
    pub visible: bool,
    pub name: Arc<str>,
    /// Frame (1-based) whose PlaceObject created this entry.
    pub placed_frame: u32,
}

#[derive(Debug)]
pub struct SpriteInst {
    /// Dictionary id of the sprite definition, `None` for the main timeline.
    pub def: Option<u16>,
    pub parent: Option<ObjId>,
    pub depth: u16,
    pub name: Arc<str>,
    pub frame: u32,
    pub playing: bool,
    pub display: Vec<Display>,
    pub material: Option<Arc<str>>,
    pub material_width: i32,
    pub material_height: i32,
    pub wait_until: Option<f64>,
    pub brightness: f32,
    /// Stop when this frame is reached while playing (the engine's play-range, 0x14174c820).
    pub stop_at: Option<u32>,
}

#[derive(Debug)]
pub struct TextInst {
    pub def: u16,
    pub parent: ObjId,
    pub depth: u16,
    pub name: Arc<str>,
    pub text: String,
    pub color: [u8; 4],
    pub align: i32,
    pub variable: String,
    pub font_fx: i32,
}

#[derive(Debug)]
pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Function(Arc<AsFunction>),
    Native(Native),
    Sprite(Box<SpriteInst>),
    Text(Box<TextInst>),
}

#[derive(Debug)]
pub struct Obj {
    pub props: Vec<(Arc<str>, Value)>,
    pub proto: Option<ObjId>,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub struct Tween {
    pub target: ObjId,
    pub prop: Arc<str>,
    pub from: f64,
    pub to: f64,
    pub duration: f64,
    pub elapsed: f64,
    pub easing: Easing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    In(Curve),
    Out(Curve),
    InOut(Curve),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    Quad,
    Cubic,
    Quart,
    Quint,
    Sine,
    Expo,
    Circ,
    Back,
    Elastic,
    Bounce,
}

impl Easing {
    pub fn parse(name: &str) -> Easing {
        let n = name.to_ascii_lowercase();
        let (kind, rest): (fn(Curve) -> Easing, &str) = if let Some(r) = n.strip_prefix("easeinout") {
            (Easing::InOut, r)
        } else if let Some(r) = n.strip_prefix("easein") {
            (Easing::In, r)
        } else if let Some(r) = n.strip_prefix("easeout") {
            (Easing::Out, r)
        } else {
            return Easing::Linear;
        };
        let c = match rest {
            "quad" => Curve::Quad,
            "cubic" => Curve::Cubic,
            "quart" => Curve::Quart,
            "quint" => Curve::Quint,
            "sine" => Curve::Sine,
            "expo" => Curve::Expo,
            "circ" => Curve::Circ,
            "back" => Curve::Back,
            "elastic" => Curve::Elastic,
            "bounce" => Curve::Bounce,
            _ => return Easing::Linear,
        };
        kind(c)
    }

    /// Robert Penner's easing equations on t in 0..1.
    pub fn apply(self, t: f64) -> f64 {
        fn ease_in(c: Curve, t: f64) -> f64 {
            use std::f64::consts::PI;
            match c {
                Curve::Quad => t * t,
                Curve::Cubic => t * t * t,
                Curve::Quart => t * t * t * t,
                Curve::Quint => t * t * t * t * t,
                Curve::Sine => 1.0 - (t * PI / 2.0).cos(),
                Curve::Expo => {
                    if t == 0.0 {
                        0.0
                    } else {
                        2f64.powf(10.0 * (t - 1.0))
                    }
                }
                Curve::Circ => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
                Curve::Back => {
                    let s = 1.70158;
                    t * t * ((s + 1.0) * t - s)
                }
                Curve::Elastic => {
                    if t == 0.0 || t == 1.0 {
                        t
                    } else {
                        let p = 0.3;
                        let s = p / 4.0;
                        -(2f64.powf(10.0 * (t - 1.0)) * ((t - 1.0 - s) * (2.0 * PI) / p).sin())
                    }
                }
                Curve::Bounce => 1.0 - bounce_out(1.0 - t),
            }
        }
        fn bounce_out(t: f64) -> f64 {
            if t < 1.0 / 2.75 {
                7.5625 * t * t
            } else if t < 2.0 / 2.75 {
                let t = t - 1.5 / 2.75;
                7.5625 * t * t + 0.75
            } else if t < 2.5 / 2.75 {
                let t = t - 2.25 / 2.75;
                7.5625 * t * t + 0.9375
            } else {
                let t = t - 2.625 / 2.75;
                7.5625 * t * t + 0.984375
            }
        }
        match self {
            Easing::Linear => t,
            Easing::In(c) => ease_in(c, t),
            Easing::Out(c) => 1.0 - ease_in(c, 1.0 - t),
            Easing::InOut(c) => {
                if t < 0.5 {
                    ease_in(c, t * 2.0) * 0.5
                } else {
                    1.0 - ease_in(c, (1.0 - t) * 2.0) * 0.5
                }
            }
        }
    }
}

/// Things the GUI asks of the game (sounds, localisation, cvars).
#[derive(Default)]
pub struct HostLog {
    pub sounds: Vec<String>,
}

pub struct Player {
    /// SWF name (e.g. "hud_bottom_left"), for messages.
    pub name: String,
    pub swf: Arc<Swf>,
    /// The SWF's own atlas (Co/A/Cg/Y), if loaded.
    pub atlas: Option<Arc<crate::texture::Texture>>,
    /// SDF fonts by `font::face_dir` key.
    pub fonts: HashMap<String, Arc<crate::font::SdfFont>>,
    pub objs: Vec<Obj>,
    pub root: ObjId,
    pub global: ObjId,
    pub sprite_proto: ObjId,
    pub text_proto: ObjId,
    pub object_proto: ObjId,
    pub array_proto: ObjId,
    /// Seconds since the player was created.
    pub time: f64,
    frame_accum: f64,
    pub tweens: Vec<Tween>,
    pub strings: Option<Arc<HashMap<String, String>>>,
    pub host: HostLog,
    pub(crate) action_queue: Vec<(ObjId, Arc<[u8]>)>,
    rng: u32,
    instance_counter: u32,
    /// Script trace output (AS2 `trace`) and runtime warnings.
    pub log: Vec<String>,
}

pub const MAX_FRAMES_PER_UPDATE: u32 = 4;

/// Diagnostics kept in `Player::log`; later messages are dropped (frame scripts can repeat every frame).
pub const LOG_CAP: usize = 256;

impl Player {
    pub(crate) fn note(&mut self, msg: String) {
        if self.log.len() < LOG_CAP {
            self.log.push(msg);
        }
    }

    pub fn new(swf: Arc<Swf>) -> Player {
        Self::new_with(swf, None, HashMap::new(), None)
    }

    pub fn new_with(
        swf: Arc<Swf>,
        atlas: Option<Arc<crate::texture::Texture>>,
        fonts: HashMap<String, Arc<crate::font::SdfFont>>,
        strings: Option<Arc<HashMap<String, String>>>,
    ) -> Player {
        let mut p = Player {
            name: String::new(),
            swf,
            atlas,
            fonts,
            objs: Vec::new(),
            root: 0,
            global: 0,
            sprite_proto: 0,
            text_proto: 0,
            object_proto: 0,
            array_proto: 0,
            time: 0.0,
            frame_accum: 0.0,
            tweens: Vec::new(),
            strings,
            host: HostLog::default(),
            action_queue: Vec::new(),
            rng: 0x1234_5678,
            instance_counter: 0,
            log: Vec::new(),
        };
        p.object_proto = p.alloc(Kind::Plain, None);
        p.global = p.alloc(Kind::Plain, Some(p.object_proto));
        p.array_proto = p.alloc(Kind::Plain, Some(p.object_proto));
        p.sprite_proto = p.alloc(Kind::Plain, Some(p.object_proto));
        p.text_proto = p.alloc(Kind::Plain, Some(p.object_proto));
        use Native::*;
        let sprite_fns: &[(&str, Native)] = &[
            ("gotoAndStop", GotoAndStop),
            ("gotoAndPlay", GotoAndPlay),
            ("play", Play),
            ("stop", Stop),
            ("nextFrame", NextFrame),
            ("prevFrame", PrevFrame),
            ("swfWait", SwfWait),
            ("swfTriggeredPause", SwfTriggeredPause),
            ("swapDepths", SwapDepths),
            ("duplicateMovieClip", DuplicateMovieClip),
            ("removeMovieClip", RemoveMovieClip),
            ("tween", Tween),
            ("removeTweens", RemoveTweens),
            ("toString", ToString),
        ];
        for (n, f) in sprite_fns {
            let o = p.alloc(Kind::Native(*f), None);
            p.set_prop(p.sprite_proto, n, Value::Obj(o));
        }
        for (n, f) in [("calcNumLines", CalcNumLines), ("toString", ToString)] {
            let o = p.alloc(Kind::Native(f), None);
            p.set_prop(p.text_proto, n, Value::Obj(o));
        }
        for (n, f) in [("push", ArrayPush), ("pop", ArrayPop), ("join", ArrayJoin), ("splice", ArraySplice), ("shift", ArrayShift), ("unshift", ArrayUnshift)] {
            let o = p.alloc(Kind::Native(f), None);
            p.set_prop(p.array_proto, n, Value::Obj(o));
        }
        let globals: &[(&str, Native)] = &[
            ("playSound", PlaySound),
            ("stopSounds", StopSounds),
            ("precacheSound", PrecacheSound),
            ("precacheFontFXMaterial", PrecacheFontFxMaterial),
            ("getPlatform", GetPlatform),
            ("getTruePlatform", GetTruePlatform),
            ("getLocalString", GetLocalString),
            ("getCVarInteger", GetCVarInteger),
            ("setCVarInteger", SetCVarInteger),
            ("strReplace", StrReplace),
            ("isJapanese", IsJapanese),
            ("acos", Acos),
            ("cos", Cos),
            ("sin", Sin),
            ("round", Round),
            ("pow", Pow),
            ("sqrt", Sqrt),
            ("abs", Abs),
            ("rand", Rand),
            ("floor", Floor),
            ("ceil", Ceil),
            ("toUpper", ToUpper),
            ("deactivate", Noop),
            ("inhibitControl", Noop),
            ("useInhibit", Noop),
            ("shortcutKeys", Noop),
            ("activateMenus", Noop),
            ("Array", ArrayCtor),
            ("Object", ObjectCtor),
        ];
        for (n, f) in globals {
            let o = p.alloc(Kind::Native(*f), None);
            p.set_prop(p.global, n, Value::Obj(o));
        }
        p.set_prop(p.global, "platform", Value::Num(2.0));
        p.set_prop(p.global, "blackbars", Value::Bool(false));
        let g = Value::Obj(p.global);
        p.set_prop(p.global, "_global", g);

        let root = p.new_sprite(None, None, 0, Arc::from("_root"));
        p.root = root;
        let r = Value::Obj(root);
        p.set_prop(p.global, "_root", r);
        // The engine runs the main timeline's first frame on load (0x141615e40 calls RunTo(1) via 0x14174dc00).
        p.run_to(root, 1);
        p.run_actions();
        p
    }

    // ---------------------------------------------------------------- heap

    pub(crate) fn alloc(&mut self, kind: Kind, proto: Option<ObjId>) -> ObjId {
        self.objs.push(Obj { props: Vec::new(), proto, kind });
        (self.objs.len() - 1) as ObjId
    }

    pub fn obj(&self, id: ObjId) -> &Obj {
        &self.objs[id as usize]
    }

    pub fn obj_mut(&mut self, id: ObjId) -> &mut Obj {
        &mut self.objs[id as usize]
    }

    pub fn sprite(&self, id: ObjId) -> Option<&SpriteInst> {
        match &self.objs.get(id as usize)?.kind {
            Kind::Sprite(s) => Some(s),
            _ => None,
        }
    }

    pub fn sprite_mut(&mut self, id: ObjId) -> Option<&mut SpriteInst> {
        match &mut self.objs.get_mut(id as usize)?.kind {
            Kind::Sprite(s) => Some(s),
            _ => None,
        }
    }

    pub fn text(&self, id: ObjId) -> Option<&TextInst> {
        match &self.objs.get(id as usize)?.kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn text_mut(&mut self, id: ObjId) -> Option<&mut TextInst> {
        match &mut self.objs.get_mut(id as usize)?.kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn get_prop(&self, id: ObjId, name: &str) -> Option<&Value> {
        self.objs[id as usize].props.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
    }

    pub fn set_prop(&mut self, id: ObjId, name: &str, v: Value) {
        let props = &mut self.objs[id as usize].props;
        if let Some(slot) = props.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
            slot.1 = v;
        } else {
            props.push((Arc::from(name), v));
        }
    }

    pub fn new_object(&mut self) -> ObjId {
        let proto = self.object_proto;
        self.alloc(Kind::Plain, Some(proto))
    }

    pub fn new_array(&mut self, items: Vec<Value>) -> ObjId {
        let proto = self.array_proto;
        self.alloc(Kind::Array(items), Some(proto))
    }

    pub(crate) fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng as f64 / u32::MAX as f64
    }

    // ---------------------------------------------------------------- display objects

    fn sprite_def(&self, def: Option<u16>) -> &Sprite {
        match def {
            None => &self.swf.main,
            Some(id) => self.swf.sprite(id).expect("sprite def"),
        }
    }

    fn new_sprite(&mut self, def: Option<u16>, parent: Option<ObjId>, depth: u16, name: Arc<str>) -> ObjId {
        let proto = self.sprite_proto;
        self.alloc(
            Kind::Sprite(Box::new(SpriteInst {
                def,
                parent,
                depth,
                name,
                frame: 0,
                playing: true,
                display: Vec::new(),
                material: None,
                material_width: 0,
                material_height: 0,
                wait_until: None,
                brightness: 0.0,
                stop_at: None,
            })),
            Some(proto),
        )
    }

    /// The display entry that holds instance `id` in its parent.
    pub fn display_of(&self, id: ObjId) -> Option<&Display> {
        let (parent, depth) = self.parent_depth(id)?;
        self.sprite(parent)?.display.iter().find(|d| d.depth == depth && d.inst == Some(id))
    }

    pub fn display_of_mut(&mut self, id: ObjId) -> Option<&mut Display> {
        let (parent, depth) = self.parent_depth(id)?;
        self.sprite_mut(parent)?.display.iter_mut().find(|d| d.depth == depth && d.inst == Some(id))
    }

    pub fn parent_depth(&self, id: ObjId) -> Option<(ObjId, u16)> {
        match &self.objs.get(id as usize)?.kind {
            Kind::Sprite(s) => Some((s.parent?, s.depth)),
            Kind::Text(t) => Some((t.parent, t.depth)),
            _ => None,
        }
    }

    pub fn parent_of(&self, id: ObjId) -> Option<ObjId> {
        self.parent_depth(id).map(|(p, _)| p)
    }

    /// A child instance of sprite `id` by instance name.
    pub fn child(&self, id: ObjId, name: &str) -> Option<ObjId> {
        self.sprite(id)?.display.iter().find(|d| d.inst.is_some() && d.name.eq_ignore_ascii_case(name)).and_then(|d| d.inst)
    }

    pub fn instance_name(&self, id: ObjId) -> Arc<str> {
        match &self.objs[id as usize].kind {
            Kind::Sprite(s) => s.name.clone(),
            Kind::Text(t) => t.name.clone(),
            _ => Arc::from(""),
        }
    }

    /// Full target path ("_root.a.b").
    pub fn target_path(&self, id: ObjId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == self.root {
                parts.push("_root".to_string());
                break;
            }
            parts.push(self.instance_name(c).to_string());
            cur = self.parent_of(c);
        }
        parts.reverse();
        parts.join(".")
    }

    /// Resolves "a.b.c" (optionally starting with `_root`/`_global`) to an object.
    pub fn find(&self, path: &str) -> Option<ObjId> {
        let mut parts = path.split(['.', '/']).filter(|s| !s.is_empty());
        let first = parts.next()?;
        let mut cur = match first {
            "_root" | "_level0" => self.root,
            "_global" => self.global,
            name => self.child(self.root, name).or_else(|| self.get_prop(self.global, name).and_then(Value::as_obj))?,
        };
        for p in parts {
            cur = self.child(cur, p).or_else(|| self.get_prop(cur, p).and_then(Value::as_obj))?;
        }
        Some(cur)
    }

    // ---------------------------------------------------------------- timeline

    pub fn frame_count(&self, id: ObjId) -> u32 {
        self.sprite(id).map(|s| self.sprite_def(s.def).frame_count as u32).unwrap_or(0)
    }

    pub fn find_label(&self, id: ObjId, label: &str) -> Option<u32> {
        let s = self.sprite(id)?;
        self.sprite_def(s.def).find_label(label)
    }

    /// Advances one sprite to `target` (1-based), applying display list changes and queueing the target
    /// frame's actions. Going backwards rebuilds the display list, keeping instances that persist.
    pub fn run_to(&mut self, id: ObjId, target: u32) {
        let Some(s) = self.sprite(id) else { return };
        let count = self.sprite_def(s.def).frame_count as u32;
        if count == 0 {
            return;
        }
        let target = target.clamp(1, count);
        let current = s.frame;
        if target == current {
            return;
        }
        let def = s.def;
        let swf = self.swf.clone();
        let sprite_def = match def {
            None => &swf.main,
            Some(d) => swf.sprite(d).unwrap(),
        };
        if target < current {
            // Rebuild: the state at `target` is the result of frames 1..=target.
            let mut want: Vec<Display> = Vec::new();
            for f in 1..=target {
                for c in sprite_def.frame_commands(f as usize - 1) {
                    match tags::decode(c) {
                        Ok(Tag::Place(po)) => apply_place_to_list(&mut want, &po, f),
                        Ok(Tag::Remove { depth }) => want.retain(|d| d.depth != depth),
                        _ => {}
                    }
                }
            }
            let old = std::mem::take(&mut self.sprite_mut(id).unwrap().display);
            let mut new_list = Vec::with_capacity(want.len());
            for mut w in want {
                if let Some(o) = old.iter().find(|o| o.depth == w.depth && o.character == w.character && o.placed_frame == w.placed_frame) {
                    w.inst = o.inst;
                    w.visible = o.visible;
                    new_list.push(w);
                } else {
                    new_list.push(w);
                }
            }
            self.sprite_mut(id).unwrap().display = new_list;
            self.sprite_mut(id).unwrap().frame = target;
            // Instantiate entries that lost (or never had) an instance.
            let missing: Vec<u16> = self.sprite(id).unwrap().display.iter().filter(|d| d.inst.is_none()).map(|d| d.depth).collect();
            for depth in missing {
                self.instantiate(id, depth);
            }
            self.queue_frame_actions(id, sprite_def, target);
            return;
        }
        for f in current + 1..=target {
            for c in sprite_def.frame_commands(f as usize - 1) {
                match tags::decode(c) {
                    Ok(Tag::Place(po)) => self.place_object(id, &po, f),
                    Ok(Tag::Remove { depth }) => self.remove_depth(id, depth),
                    _ => {}
                }
            }
        }
        self.sprite_mut(id).unwrap().frame = target;
        self.queue_frame_actions(id, sprite_def, target);
    }

    fn queue_frame_actions(&mut self, id: ObjId, def: &Sprite, frame: u32) {
        for c in def.frame_commands(frame as usize - 1) {
            if c.tag == tags::TAG_DO_ACTION {
                self.action_queue.push((id, Arc::from(c.data.as_slice())));
            }
        }
    }

    fn place_object(&mut self, id: ObjId, po: &PlaceObject, frame: u32) {
        let exists = self.sprite(id).unwrap().display.iter().any(|d| d.depth == po.depth);
        if !po.is_move && po.character.is_some() {
            if exists {
                self.note(format!("{}: PlaceObject at occupied depth {}", self.target_path(id), po.depth));
                return;
            }
        } else if !po.is_move || !exists {
            return;
        }
        let new_char = {
            let list = &mut self.sprite_mut(id).unwrap().display;
            let before = list.iter().find(|d| d.depth == po.depth).map(|d| (d.character, d.inst.is_some()));
            apply_place_to_list(list, po, frame);
            match before {
                None => true,
                Some((c, has_inst)) => po.character.is_some_and(|nc| nc != c && !has_inst),
            }
        };
        let depth = po.depth;
        if new_char {
            self.instantiate(id, depth);
        } else if let Some(name) = &po.name {
            // Renaming an existing instance.
            if let Some(inst) = self.sprite(id).unwrap().display.iter().find(|d| d.depth == depth).and_then(|d| d.inst) {
                match &mut self.obj_mut(inst).kind {
                    Kind::Sprite(s) => s.name = Arc::from(name.as_str()),
                    Kind::Text(t) => t.name = Arc::from(name.as_str()),
                    _ => {}
                }
            }
        }
    }

    /// Creates the sprite / edit-text instance for the display entry at `depth`.
    fn instantiate(&mut self, parent: ObjId, depth: u16) {
        let Some(d) = self.sprite(parent).unwrap().display.iter().find(|d| d.depth == depth) else { return };
        let character = d.character;
        let mut name = d.name.clone();
        if name.is_empty() {
            self.instance_counter += 1;
            name = Arc::from(format!("instance{}", self.instance_counter).as_str());
        }
        let swf = self.swf.clone();
        let inst = match swf.dict.get(character as usize) {
            Some(DictEntry::Sprite(_)) => {
                let s = self.new_sprite(Some(character), Some(parent), depth, name.clone());
                Some(s)
            }
            Some(DictEntry::EditText(et)) => {
                let proto = self.text_proto;
                let t = self.alloc(
                    Kind::Text(Box::new(TextInst {
                        def: character,
                        parent,
                        depth,
                        name: name.clone(),
                        text: et.initial_text.clone(),
                        color: et.color,
                        align: et.align,
                        variable: et.variable.clone(),
                        font_fx: 0,
                    })),
                    Some(proto),
                );
                Some(t)
            }
            _ => None,
        };
        if let Some(d) = self.sprite_mut(parent).unwrap().display.iter_mut().find(|d| d.depth == depth) {
            d.inst = inst;
            d.name = name;
        }
        if let Some(i) = inst {
            if self.sprite(i).is_some() {
                // New clips show their first frame immediately.
                self.run_to(i, 1);
            }
        }
    }

    fn remove_depth(&mut self, id: ObjId, depth: u16) {
        let removed: Vec<ObjId> = {
            let list = &mut self.sprite_mut(id).unwrap().display;
            let r = list.iter().filter(|d| d.depth == depth).filter_map(|d| d.inst).collect();
            list.retain(|d| d.depth != depth);
            r
        };
        if !removed.is_empty() {
            self.tweens.retain(|t| !removed.contains(&t.target));
        }
    }

    /// Steps every playing timeline by one frame (depth-first, parents before children).
    pub fn step_frame(&mut self) {
        let mut stack = vec![self.root];
        let mut order = Vec::new();
        while let Some(id) = stack.pop() {
            order.push(id);
            if let Some(s) = self.sprite(id) {
                for d in s.display.iter().rev() {
                    if let Some(i) = d.inst {
                        if self.sprite(i).is_some() {
                            stack.push(i);
                        }
                    }
                }
            }
        }
        for id in order {
            let Some(s) = self.sprite(id) else { continue };
            if let Some(until) = s.wait_until {
                if self.time < until {
                    continue;
                }
                let s = self.sprite_mut(id).unwrap();
                s.wait_until = None;
                s.playing = true;
            }
            let s = self.sprite(id).unwrap();
            if !s.playing {
                continue;
            }
            let count = self.frame_count(id);
            let s = self.sprite(id).unwrap();
            if count <= 1 && s.frame >= 1 {
                continue;
            }
            let next = if s.frame >= count { 1 } else { s.frame + 1 };
            let stop_at = s.stop_at;
            self.run_to(id, next);
            if stop_at == Some(next) {
                if let Some(s) = self.sprite_mut(id) {
                    s.playing = false;
                    s.stop_at = None;
                }
            }
        }
        self.run_actions();
    }

    /// Shows frame `from` and plays forward until `to` (both 1-based).
    pub fn play_range(&mut self, id: ObjId, from: u32, to: u32) {
        self.goto(id, Value::Num(from as f64), from != to);
        if let Some(s) = self.sprite_mut(id) {
            s.stop_at = if from != to { Some(to) } else { None };
        }
        self.run_actions();
    }

    /// Advances the GUI by `dt` seconds: timelines at the SWF frame rate, tweens in real time.
    pub fn update(&mut self, dt: f64) {
        self.time += dt;
        self.update_tweens(dt);
        let fps = self.swf.frames_per_second().max(1.0) as f64;
        self.frame_accum += dt * fps;
        let mut n = 0;
        while self.frame_accum >= 1.0 && n < MAX_FRAMES_PER_UPDATE {
            self.frame_accum -= 1.0;
            self.step_frame();
            n += 1;
        }
        if self.frame_accum > 1.0 {
            self.frame_accum = 0.0;
        }
    }

    pub fn run_actions(&mut self) {
        let mut guard = 0;
        while !self.action_queue.is_empty() && guard < 64 {
            guard += 1;
            let queue = std::mem::take(&mut self.action_queue);
            for (target, code) in queue {
                // Actions of clips removed meanwhile are dropped.
                if target != self.root && self.display_of(target).is_none() {
                    continue;
                }
                if let Err(e) = self.run_code(target, code) {
                    let path = self.target_path(target);
                    self.note(format!("{path}: {e}"));
                }
            }
        }
    }

    fn update_tweens(&mut self, dt: f64) {
        if self.tweens.is_empty() {
            return;
        }
        let mut tweens = std::mem::take(&mut self.tweens);
        for t in tweens.iter_mut() {
            t.elapsed += dt;
            if t.elapsed < 0.0 {
                continue;
            }
            let k = if t.duration <= 0.0 { 1.0 } else { (t.elapsed / t.duration).min(1.0) };
            let v = t.from + (t.to - t.from) * t.easing.apply(k);
            self.set_member(Value::Obj(t.target), &t.prop.clone(), Value::Num(v));
        }
        tweens.retain(|t| t.elapsed < t.duration);
        // Tweens started by setters during this update are appended after the kept ones.
        tweens.append(&mut self.tweens);
        self.tweens = tweens;
    }

    pub fn add_tween(&mut self, target: ObjId, prop: &str, from: f64, to: f64, duration: f64, easing: Easing, delay: f64) {
        self.tweens.retain(|t| !(t.target == target && t.prop.eq_ignore_ascii_case(prop)));
        let tw = Tween { target, prop: Arc::from(prop), from, to, duration, elapsed: -delay, easing };
        if delay <= 0.0 {
            self.set_member(Value::Obj(target), prop, Value::Num(from));
        }
        self.tweens.push(tw);
    }

    // ---------------------------------------------------------------- game-facing helpers

    /// Sets an edit text's text (`txtVal.text = s`).
    pub fn set_text(&mut self, path: &str, s: &str) -> bool {
        let Some(id) = self.find(path) else { return false };
        match self.text_mut(id) {
            Some(t) => {
                t.text = s.to_string();
                true
            }
            None => false,
        }
    }

    pub fn goto_and_stop(&mut self, path: &str, frame: impl Into<Value>) -> bool {
        let Some(id) = self.find(path) else { return false };
        self.goto(id, frame.into(), false);
        self.run_actions();
        true
    }

    pub fn goto_and_play(&mut self, path: &str, frame: impl Into<Value>) -> bool {
        let Some(id) = self.find(path) else { return false };
        self.goto(id, frame.into(), true);
        self.run_actions();
        true
    }

    pub(crate) fn goto(&mut self, id: ObjId, frame: Value, play: bool) {
        let target = match &frame {
            Value::Str(s) => match s.parse::<f64>() {
                Ok(n) => n as u32,
                Err(_) => match self.find_label(id, s) {
                    Some(f) => f,
                    None => {
                        self.note(format!("{}: no frame label '{s}'", self.target_path(id)));
                        return;
                    }
                },
            },
            v => self.to_number(v).max(1.0) as u32,
        };
        if let Some(s) = self.sprite_mut(id) {
            s.playing = play;
            s.wait_until = None;
            s.stop_at = None;
        }
        self.run_to(id, target);
    }

    pub fn set(&mut self, path: &str, member: &str, v: impl Into<Value>) -> bool {
        let Some(id) = self.find(path) else { return false };
        self.set_member(Value::Obj(id), member, v.into());
        true
    }

    pub fn get(&mut self, path: &str, member: &str) -> Value {
        match self.find(path) {
            Some(id) => self.get_member(Value::Obj(id), member),
            None => Value::Undefined,
        }
    }

    /// Calls a script function stored at `path` ("_global.setAmmoCount", "_root.mc.fireKick") with `this` set
    /// to the object holding it.
    pub fn call(&mut self, path: &str, args: &[Value]) -> Value {
        let (holder, name) = match path.rsplit_once('.') {
            Some((h, n)) => (self.find(h), n),
            None => (Some(self.root), path),
        };
        let Some(holder) = holder else { return Value::Undefined };
        let f = self.get_member(Value::Obj(holder), name);
        let this = if holder == self.global { Value::Obj(self.root) } else { Value::Obj(holder) };
        match self.call_value(f, this, args.to_vec()) {
            Ok(v) => {
                self.run_actions();
                v
            }
            Err(e) => {
                self.note(format!("call {path}: {e}"));
                Value::Undefined
            }
        }
    }

    pub fn font(&self, face: &str) -> Option<&crate::font::SdfFont> {
        self.fonts.get(&crate::font::face_dir(face)).map(|f| f.as_ref())
    }

    /// Indented listing of the live instance tree (for debugging).
    pub fn dump_tree(&self) -> String {
        let mut out = String::new();
        self.dump_rec(self.root, 0, &mut out);
        out
    }

    fn dump_rec(&self, id: ObjId, indent: usize, out: &mut String) {
        use std::fmt::Write;
        let Some(s) = self.sprite(id) else { return };
        let _ = writeln!(out, "{}{} [frame {}/{}{}]", "  ".repeat(indent), s.name, s.frame, self.frame_count(id), if s.playing { " playing" } else { "" });
        for d in &s.display {
            let kind = self.swf.dict.get(d.character as usize).map(|e| e.type_name()).unwrap_or("?");
            let m = &d.matrix;
            let _ = writeln!(
                out,
                "{}d{} #{} {} '{}' vis={} a={:.2} pos=({:.1},{:.1}) s=({:.2},{:.2}){}{}",
                "  ".repeat(indent + 1),
                d.depth,
                d.character,
                kind,
                d.name,
                d.visible,
                d.cxform.mul[3],
                m.tx,
                m.ty,
                m.xx,
                m.yy,
                if d.clip_depth > 0 { format!(" clip{}", d.clip_depth) } else { String::new() },
                if d.blend > 1 { format!(" blend{}", d.blend) } else { String::new() }
            );
            if let Some(i) = d.inst {
                if let Some(t) = self.text(i) {
                    let _ = writeln!(out, "{}text {:?}", "  ".repeat(indent + 2), t.text);
                } else {
                    self.dump_rec(i, indent + 2, out);
                }
            }
        }
    }

    /// Localised string for `#str_...` keys (falls back to the key).
    pub fn localize<'a>(&'a self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if s.starts_with('#') {
            if let Some(map) = &self.strings {
                if let Some(v) = map.get(&s.to_ascii_lowercase()) {
                    return std::borrow::Cow::Owned(v.clone());
                }
            }
        }
        std::borrow::Cow::Borrowed(s)
    }
}

fn apply_place_to_list(list: &mut Vec<Display>, po: &PlaceObject, frame: u32) {
    let idx = list.iter().position(|d| d.depth == po.depth);
    let d = match idx {
        Some(i) => {
            if let Some(c) = po.character {
                if !po.is_move || list[i].inst.is_none() {
                    list[i].character = c;
                }
            }
            &mut list[i]
        }
        None => {
            let Some(character) = po.character else { return };
            let pos = list.partition_point(|d| d.depth < po.depth);
            list.insert(
                pos,
                Display {
                    depth: po.depth,
                    character,
                    inst: None,
                    matrix: Matrix::IDENTITY,
                    cxform: ColorXform::IDENTITY,
                    ratio: 0.0,
                    clip_depth: 0,
                    blend: 0,
                    visible: true,
                    name: Arc::from(""),
                    placed_frame: frame,
                },
            );
            &mut list[pos]
        }
    };
    if let Some(m) = po.matrix {
        d.matrix = m;
    }
    if let Some(c) = po.cxform {
        d.cxform = c;
    }
    if let Some(r) = po.ratio {
        d.ratio = r as f32 / 65535.0;
    }
    if let Some(n) = &po.name {
        d.name = Arc::from(n.as_str());
    }
    if let Some(c) = po.clip_depth {
        d.clip_depth = c;
    }
    if let Some(b) = po.blend_mode {
        d.blend = b;
    }
    if let Some(v) = po.visible {
        d.visible = v;
    }
}

/// Decrypted `strings/<language>.bfile` contents: lines of `"key"<tab>"value"`.
pub fn parse_lang(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else { continue };
        let Some(end) = rest.find('"') else { continue };
        let key = &rest[..end];
        let after = rest[end + 1..].trim_start();
        let Some(val) = after.strip_prefix('"') else { continue };
        // The value ends at its closing quote; some lines carry further fields (`"LOW HEALTH" "21" "32"`).
        let mut out = String::with_capacity(val.len());
        let mut chars = val.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o) => out.push(o),
                    None => out.push('\\'),
                },
                c => out.push(c),
            }
        }
        map.insert(key.to_ascii_lowercase(), out);
    }
    map
}

/// Loads the English string table from the install.
pub fn load_strings(container: &idres::Container) -> anyhow::Result<HashMap<String, String>> {
    let e = container.get("generated/binaryfile/strings/english.bfile").ok_or_else(|| anyhow::anyhow!("english strings missing"))?;
    let raw = container.read(e)?;
    let bytes = idres::crypt::decrypt(&raw, &e.short_name).ok_or_else(|| anyhow::anyhow!("decrypting strings"))?;
    Ok(parse_lang(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn lang_value_stops_at_closing_quote() {
        let m = super::parse_lang("\t\"#str_low_health\"\t\"LOW HEALTH\"\t\"21\"\t\"32\"\n\"#str_a\" \"say \\\"hi\\\"\\nnow\"\n");
        assert_eq!(m["#str_low_health"], "LOW HEALTH");
        assert_eq!(m["#str_a"], "say \"hi\"\nnow");
    }
}
