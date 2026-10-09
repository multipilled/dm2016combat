//! Typed HIRC objects for bank version 113 (Wwise 2016.1).
//!
//! Every Sound / Action(Play) / Event / RanSeq / Switch / Layer / ActorMixer object in the install
//! parses with zero bytes left over (checked by `sndx check`), which is how the layouts below were
//! confirmed. Field names follow the Wwise SDK where known; `unk_*` fields have only been seen as 0.

use anyhow::{Result, bail, ensure};

use crate::read::Cursor;

/// HIRC object types.
pub mod kind {
    pub const STATE: u8 = 1;
    pub const SOUND: u8 = 2;
    pub const ACTION: u8 = 3;
    pub const EVENT: u8 = 4;
    pub const RAN_SEQ: u8 = 5;
    pub const SWITCH: u8 = 6;
    pub const ACTOR_MIXER: u8 = 7;
    pub const BUS: u8 = 8;
    pub const LAYER: u8 = 9;
    pub const MUSIC_SEGMENT: u8 = 10;
    pub const MUSIC_TRACK: u8 = 11;
    pub const MUSIC_SWITCH: u8 = 12;
    pub const MUSIC_RAN_SEQ: u8 = 13;
    pub const ATTENUATION: u8 = 14;
    pub const FX_SHARE_SET: u8 = 18;
    pub const FX_CUSTOM: u8 = 19;
    pub const AUX_BUS: u8 = 20;

    pub fn name(k: u8) -> &'static str {
        match k {
            1 => "State",
            2 => "Sound",
            3 => "Action",
            4 => "Event",
            5 => "RanSeqCntr",
            6 => "SwitchCntr",
            7 => "ActorMixer",
            8 => "Bus",
            9 => "LayerCntr",
            10 => "MusicSegment",
            11 => "MusicTrack",
            12 => "MusicSwitchCntr",
            13 => "MusicRanSeqCntr",
            14 => "Attenuation",
            15 => "DialogueEvent",
            16 => "FeedbackBus",
            17 => "FeedbackNode",
            18 => "FxShareSet",
            19 => "FxCustom",
            20 => "AuxBus",
            21 => "LFO",
            22 => "Envelope",
            _ => "?",
        }
    }
}

/// `AkPropID` values seen in the install. Confirmed = checked against soundbanksinfo durations or
/// value ranges that admit one reading; the rest follow the Wwise 2016 enum order (14/15/16 line up).
pub mod prop {
    /// dB, float.
    pub const VOLUME: u8 = 0;
    pub const LFE: u8 = 1;
    /// cents, float.
    pub const PITCH: u8 = 2;
    /// 0..100, float.
    pub const LPF: u8 = 3;
    /// 0..100, float (inferred: 2016.1 added the high-pass filter; values 3..43).
    pub const HPF: u8 = 4;
    /// dB, float (buses; -6 on Master Audio Bus).
    pub const BUS_VOLUME: u8 = 5;
    pub const PRIORITY: u8 = 6;
    pub const PRIORITY_DISTANCE_OFFSET: u8 = 7;
    pub const PAN_LR: u8 = 11;
    pub const PAN_FR: u8 = 12;
    pub const CENTER_PCT: u8 = 13;
    /// ms, integer (actions).
    pub const DELAY_TIME: u8 = 14;
    /// ms, integer (actions: fade time).
    pub const TRANSITION_TIME: u8 = 15;
    /// percent, float (actions).
    pub const PROBABILITY: u8 = 16;
    pub const USER_AUX_SEND_VOLUME0: u8 = 18;
    /// loop count, integer: 0 = infinite, 1 = once (confirmed: these events are `Infinite` in the XML).
    pub const LOOP: u8 = 58;
    /// seconds, float (confirmed: event durations = media + delay).
    pub const INITIAL_DELAY: u8 = 59;
}

#[derive(Debug, Clone, Copy)]
pub struct FxSlot {
    pub index: u8,
    pub fx_id: u32,
    pub is_share_set: bool,
    pub is_rendered: bool,
}

#[derive(Debug, Clone)]
pub struct PathData {
    pub mode: u8,
    pub transition_ms: i32,
    /// x, y, z, duration (ms).
    pub vertices: Vec<([f32; 3], i32)>,
    /// (first vertex, vertex count), with a random range per item.
    pub playlist: Vec<(u32, u32, [f32; 3])>,
}

#[derive(Debug, Clone, Default)]
pub struct Positioning {
    /// bit0 override parent, bit3 3D (others seen: 0x02, 0x04, 0x10, 0x20, 0x40, 0x80).
    pub bits: u8,
    /// bit0 set = game-defined position; clear = user-defined path (then `path` is present).
    pub bits_3d: Option<u8>,
    pub attenuation: Option<u32>,
    pub path: Option<PathData>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AdvSettings {
    pub bits: u8,
    pub virtual_queue: u8,
    pub max_instances: u16,
    pub below_threshold: u8,
    pub bits2: u8,
}

#[derive(Debug, Clone)]
pub struct StateGroup {
    pub group: u32,
    pub sync_type: u8,
    /// (state id, state-object id)
    pub states: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct RtpcCurve {
    pub rtpc: u32,
    pub rtpc_type: u8,
    pub accum: u8,
    pub param: u8,
    pub curve_id: u32,
    pub scaling: u8,
    /// (from, to, interpolation)
    pub points: Vec<(f32, f32, u32)>,
}

/// `NodeBaseParams`, shared by sounds and containers.
#[derive(Debug, Clone, Default)]
pub struct NodeBase {
    pub fx_override: bool,
    pub fx_bypass: u8,
    pub fx: Vec<FxSlot>,
    pub unk_byte: u8,
    pub override_bus: u32,
    pub parent: u32,
    pub priority_bits: u8,
    /// (prop id, raw 32-bit value; float or integer depending on the prop)
    pub props: Vec<(u8, u32)>,
    /// (prop id, min, max) random modifiers
    pub ranged: Vec<(u8, f32, f32)>,
    pub positioning: Positioning,
    pub aux_bits: u8,
    pub aux_buses: [u32; 4],
    pub adv: AdvSettings,
    pub states: Vec<StateGroup>,
    pub rtpcs: Vec<RtpcCurve>,
}

pub fn prop_raw(props: &[(u8, u32)], id: u8) -> Option<u32> {
    props.iter().find(|(p, _)| *p == id).map(|(_, v)| *v)
}

impl NodeBase {
    pub fn prop_f32(&self, id: u8) -> Option<f32> {
        prop_raw(&self.props, id).map(f32::from_bits)
    }
    pub fn prop_u32(&self, id: u8) -> Option<u32> {
        prop_raw(&self.props, id)
    }
    pub fn range(&self, id: u8) -> Option<(f32, f32)> {
        self.ranged.iter().find(|(p, ..)| *p == id).map(|&(_, a, b)| (a, b))
    }
}

/// `AkBankSourceData`.
#[derive(Debug, Clone)]
pub struct Source {
    /// 0x00020001 = ADPCM codec; 0x00650002 = Silence source, 0x00640002 = Sine, 0x00940002 = (synth).
    pub plugin_id: u32,
    /// 0 = in bank (DIDX), 1 = prefetched stream, 2 = streamed (.pck).
    pub stream_type: u8,
    pub source_id: u32,
    pub in_memory_size: u32,
    pub source_bits: u8,
    /// Source-plugin parameter block (only when `plugin_id & 0xf == 2`).
    pub plugin_params: Vec<u8>,
}

impl Source {
    pub fn is_codec(&self) -> bool {
        self.plugin_id & 0xf == 1
    }
}

#[derive(Debug, Clone)]
pub struct Sound {
    pub source: Source,
    pub node: NodeBase,
}

pub mod action {
    pub const STOP_E: u16 = 0x0102;
    pub const STOP_E_O: u16 = 0x0103;
    pub const PAUSE_E: u16 = 0x0202;
    pub const RESUME_E: u16 = 0x0302;
    pub const PLAY: u16 = 0x0403;
    pub const PLAY_AND_CONTINUE: u16 = 0x0503;
    pub const SET_STATE: u16 = 0x1204;
}

#[derive(Debug, Clone)]
pub enum ActionParams {
    Play { fade_curve: u8, bank_id: u32 },
    /// Stop/Pause/Resume: fade curve, then the exception list.
    Active { fade_curve: u8, rest: Vec<u8> },
    SetState { group: u32, state: u32 },
    Other(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct Action {
    pub action_type: u16,
    pub target: u32,
    pub target_is_bus: bool,
    pub props: Vec<(u8, u32)>,
    pub ranged: Vec<(u8, f32, f32)>,
    pub params: ActionParams,
}

impl Action {
    pub fn prop_u32(&self, id: u8) -> Option<u32> {
        prop_raw(&self.props, id)
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub actions: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct RanSeq {
    pub node: NodeBase,
    /// 0 = infinite (continuous mode only).
    pub loop_count: u16,
    pub loop_mod_min: u16,
    pub loop_mod_max: u16,
    pub transition_time: f32,
    pub transition_mod_min: f32,
    pub transition_mod_max: f32,
    pub avoid_repeat: u16,
    pub transition_mode: u8,
    /// 0 = standard, 1 = shuffle.
    pub random_mode: u8,
    /// 0 = random, 1 = sequence.
    pub mode: u8,
    /// See the `RANSEQ_*` bits.
    pub bits: u8,
    pub children: Vec<u32>,
    /// (child, weight); weights only matter with [`RANSEQ_USING_WEIGHT`].
    pub playlist: Vec<(u32, i32)>,
}

pub const RANSEQ_USING_WEIGHT: u8 = 0x01;
pub const RANSEQ_RESET_PLAYLIST: u8 = 0x02;
pub const RANSEQ_RESTART_BACKWARD: u8 = 0x04;
pub const RANSEQ_CONTINUOUS: u8 = 0x08;
pub const RANSEQ_GLOBAL: u8 = 0x10;

#[derive(Debug, Clone, Copy)]
pub struct SwitchNodeParams {
    pub node: u32,
    pub bits: u8,
    pub on_switch_mode: u8,
    pub fade_out_ms: i32,
    pub fade_in_ms: i32,
}

#[derive(Debug, Clone)]
pub struct Switch {
    pub node: NodeBase,
    /// 0 = switch group, 1 = state group.
    pub group_type: u8,
    pub group: u32,
    pub default_switch: u32,
    pub continuous_validation: bool,
    pub children: Vec<u32>,
    /// (switch value, children played for it)
    pub switches: Vec<(u32, Vec<u32>)>,
    pub params: Vec<SwitchNodeParams>,
}

#[derive(Debug, Clone)]
pub struct LayerDef {
    pub id: u32,
    pub rtpcs: Vec<RtpcCurve>,
    pub crossfade_rtpc: u32,
    pub crossfade_rtpc_type: u8,
    /// (child, crossfade curve points)
    pub assocs: Vec<(u32, Vec<(f32, f32, u32)>)>,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub node: NodeBase,
    pub children: Vec<u32>,
    pub layers: Vec<LayerDef>,
}

#[derive(Debug, Clone)]
pub struct ActorMixer {
    pub node: NodeBase,
    pub children: Vec<u32>,
}

#[derive(Debug, Clone)]
pub enum Object {
    Sound(Sound),
    Action(Action),
    Event(Event),
    RanSeq(RanSeq),
    Switch(Switch),
    Layer(Layer),
    ActorMixer(ActorMixer),
    /// Types not decoded (buses, music, attenuations, effects, ...).
    Other(u8),
}

impl Object {
    pub fn node(&self) -> Option<&NodeBase> {
        match self {
            Object::Sound(s) => Some(&s.node),
            Object::RanSeq(c) => Some(&c.node),
            Object::Switch(c) => Some(&c.node),
            Object::Layer(c) => Some(&c.node),
            Object::ActorMixer(c) => Some(&c.node),
            _ => None,
        }
    }

    pub fn children(&self) -> &[u32] {
        match self {
            Object::RanSeq(c) => &c.children,
            Object::Switch(c) => &c.children,
            Object::Layer(c) => &c.children,
            Object::ActorMixer(c) => &c.children,
            _ => &[],
        }
    }

    pub fn kind(&self) -> u8 {
        match self {
            Object::Sound(_) => kind::SOUND,
            Object::Action(_) => kind::ACTION,
            Object::Event(_) => kind::EVENT,
            Object::RanSeq(_) => kind::RAN_SEQ,
            Object::Switch(_) => kind::SWITCH,
            Object::Layer(_) => kind::LAYER,
            Object::ActorMixer(_) => kind::ACTOR_MIXER,
            Object::Other(k) => *k,
        }
    }
}

/// Parses one HIRC object body (after its id). Decoded kinds must consume the body exactly.
pub fn parse(kind: u8, body: &[u8]) -> Result<Object> {
    let mut r = Cursor::new(body);
    let obj = match kind {
        kind::SOUND => Object::Sound(Sound { source: source(&mut r)?, node: node_base(&mut r)? }),
        kind::ACTION => Object::Action(action(&mut r)?),
        kind::EVENT => {
            let n = r.u32()?;
            Object::Event(Event { actions: (0..n).map(|_| r.u32()).collect::<Result<_>>()? })
        }
        kind::RAN_SEQ => Object::RanSeq(ran_seq(&mut r)?),
        kind::SWITCH => Object::Switch(switch(&mut r)?),
        kind::LAYER => Object::Layer(layer(&mut r)?),
        kind::ACTOR_MIXER => Object::ActorMixer(ActorMixer { node: node_base(&mut r)?, children: children(&mut r)? }),
        k => return Ok(Object::Other(k)),
    };
    ensure!(r.remaining() == 0, "{} has {} unparsed bytes", kind::name(kind), r.remaining());
    Ok(obj)
}

fn source(r: &mut Cursor) -> Result<Source> {
    let plugin_id = r.u32()?;
    let stream_type = r.u8()?;
    let source_id = r.u32()?;
    let in_memory_size = r.u32()?;
    let source_bits = r.u8()?;
    let plugin_params = if plugin_id & 0xf == 2 {
        let n = r.u32()? as usize;
        r.bytes(n)?.to_vec()
    } else {
        Vec::new()
    };
    Ok(Source { plugin_id, stream_type, source_id, in_memory_size, source_bits, plugin_params })
}

fn props(r: &mut Cursor) -> Result<Vec<(u8, u32)>> {
    let n = r.u8()? as usize;
    let ids = r.bytes(n)?.to_vec();
    ids.into_iter().map(|id| Ok((id, r.u32()?))).collect()
}

fn ranged(r: &mut Cursor) -> Result<Vec<(u8, f32, f32)>> {
    let n = r.u8()? as usize;
    let ids = r.bytes(n)?.to_vec();
    ids.into_iter().map(|id| Ok((id, r.f32()?, r.f32()?))).collect()
}

fn points(r: &mut Cursor, n: usize) -> Result<Vec<(f32, f32, u32)>> {
    (0..n).map(|_| Ok((r.f32()?, r.f32()?, r.u32()?))).collect()
}

fn rtpcs(r: &mut Cursor) -> Result<Vec<RtpcCurve>> {
    let n = r.u16()?;
    (0..n)
        .map(|_| {
            let rtpc = r.u32()?;
            let rtpc_type = r.u8()?;
            let accum = r.u8()?;
            let param = r.u8()?;
            let curve_id = r.u32()?;
            let scaling = r.u8()?;
            let count = r.u16()? as usize;
            Ok(RtpcCurve { rtpc, rtpc_type, accum, param, curve_id, scaling, points: points(r, count)? })
        })
        .collect()
}

fn positioning(r: &mut Cursor) -> Result<Positioning> {
    let bits = r.u8()?;
    let mut p = Positioning { bits, ..Default::default() };
    if bits & 0x01 != 0 && bits & 0x08 != 0 {
        let b3 = r.u8()?;
        p.bits_3d = Some(b3);
        p.attenuation = Some(r.u32()?);
        if b3 & 0x01 == 0 {
            let mode = r.u8()?;
            let transition_ms = r.i32()?;
            let nv = r.u32()? as usize;
            let vertices = (0..nv).map(|_| Ok(([r.f32()?, r.f32()?, r.f32()?], r.i32()?))).collect::<Result<Vec<_>>>()?;
            let np = r.u32()? as usize;
            let items = (0..np).map(|_| Ok((r.u32()?, r.u32()?))).collect::<Result<Vec<_>>>()?;
            let playlist = items
                .into_iter()
                .map(|(a, b)| Ok((a, b, [r.f32()?, r.f32()?, r.f32()?])))
                .collect::<Result<Vec<_>>>()?;
            p.path = Some(PathData { mode, transition_ms, vertices, playlist });
        }
    }
    Ok(p)
}

fn node_base(r: &mut Cursor) -> Result<NodeBase> {
    let mut n = NodeBase { fx_override: r.u8()? != 0, ..Default::default() };
    let num_fx = r.u8()?;
    if num_fx > 0 {
        n.fx_bypass = r.u8()?;
    }
    for _ in 0..num_fx {
        n.fx.push(FxSlot { index: r.u8()?, fx_id: r.u32()?, is_share_set: r.u8()? != 0, is_rendered: r.u8()? != 0 });
    }
    n.unk_byte = r.u8()?;
    n.override_bus = r.u32()?;
    n.parent = r.u32()?;
    n.priority_bits = r.u8()?;
    n.props = props(r)?;
    n.ranged = ranged(r)?;
    n.positioning = positioning(r)?;
    n.aux_bits = r.u8()?;
    if n.aux_bits & 0x08 != 0 {
        for bus in &mut n.aux_buses {
            *bus = r.u32()?;
        }
    }
    n.adv = AdvSettings { bits: r.u8()?, virtual_queue: r.u8()?, max_instances: r.u16()?, below_threshold: r.u8()?, bits2: r.u8()? };
    let num_groups = r.u32()?;
    for _ in 0..num_groups {
        let group = r.u32()?;
        let sync_type = r.u8()?;
        let k = r.u16()?;
        let states = (0..k).map(|_| Ok((r.u32()?, r.u32()?))).collect::<Result<_>>()?;
        n.states.push(StateGroup { group, sync_type, states });
    }
    n.rtpcs = rtpcs(r)?;
    Ok(n)
}

fn children(r: &mut Cursor) -> Result<Vec<u32>> {
    let n = r.u32()?;
    (0..n).map(|_| r.u32()).collect()
}

fn action(r: &mut Cursor) -> Result<Action> {
    let action_type = r.u16()?;
    let target = r.u32()?;
    let target_is_bus = r.u8()? != 0;
    let props = props(r)?;
    let ranged = ranged(r)?;
    let params = match action_type {
        action::PLAY | action::PLAY_AND_CONTINUE => ActionParams::Play { fade_curve: r.u8()?, bank_id: r.u32()? },
        0x0100..=0x03ff => {
            let fade_curve = r.u8()?;
            ActionParams::Active { fade_curve, rest: r.bytes(r.remaining())?.to_vec() }
        }
        action::SET_STATE if r.remaining() == 8 => ActionParams::SetState { group: r.u32()?, state: r.u32()? },
        _ => ActionParams::Other(r.bytes(r.remaining())?.to_vec()),
    };
    Ok(Action { action_type, target, target_is_bus, props, ranged, params })
}

fn ran_seq(r: &mut Cursor) -> Result<RanSeq> {
    let node = node_base(r)?;
    let loop_count = r.u16()?;
    let loop_mod_min = r.u16()?;
    let loop_mod_max = r.u16()?;
    let transition_time = r.f32()?;
    let transition_mod_min = r.f32()?;
    let transition_mod_max = r.f32()?;
    let avoid_repeat = r.u16()?;
    let transition_mode = r.u8()?;
    let random_mode = r.u8()?;
    let mode = r.u8()?;
    let bits = r.u8()?;
    let children = children(r)?;
    let n = r.u16()?;
    let playlist = (0..n).map(|_| Ok((r.u32()?, r.i32()?))).collect::<Result<_>>()?;
    Ok(RanSeq {
        node,
        loop_count,
        loop_mod_min,
        loop_mod_max,
        transition_time,
        transition_mod_min,
        transition_mod_max,
        avoid_repeat,
        transition_mode,
        random_mode,
        mode,
        bits,
        children,
        playlist,
    })
}

fn switch(r: &mut Cursor) -> Result<Switch> {
    let node = node_base(r)?;
    let group_type = r.u8()?;
    let group = r.u32()?;
    let default_switch = r.u32()?;
    let continuous_validation = r.u8()? != 0;
    let children = children(r)?;
    let ng = r.u32()?;
    let switches = (0..ng)
        .map(|_| {
            let value = r.u32()?;
            let k = r.u32()?;
            Ok((value, (0..k).map(|_| r.u32()).collect::<Result<Vec<_>>>()?))
        })
        .collect::<Result<_>>()?;
    let np = r.u32()?;
    let params = (0..np)
        .map(|_| {
            Ok(SwitchNodeParams { node: r.u32()?, bits: r.u8()?, on_switch_mode: r.u8()?, fade_out_ms: r.i32()?, fade_in_ms: r.i32()? })
        })
        .collect::<Result<_>>()?;
    if group_type > 1 {
        bail!("switch container group type {group_type}");
    }
    Ok(Switch { node, group_type, group, default_switch, continuous_validation, children, switches, params })
}

fn layer(r: &mut Cursor) -> Result<Layer> {
    let node = node_base(r)?;
    let children = children(r)?;
    let nl = r.u32()?;
    let layers = (0..nl)
        .map(|_| {
            let id = r.u32()?;
            let rtpcs = rtpcs(r)?;
            let crossfade_rtpc = r.u32()?;
            let crossfade_rtpc_type = r.u8()?;
            let na = r.u32()?;
            let assocs = (0..na)
                .map(|_| {
                    let child = r.u32()?;
                    let k = r.u32()? as usize;
                    Ok((child, points(r, k)?))
                })
                .collect::<Result<_>>()?;
            Ok(LayerDef { id, rtpcs, crossfade_rtpc, crossfade_rtpc_type, assocs })
        })
        .collect::<Result<_>>()?;
    Ok(Layer { node, children, layers })
}

impl RtpcCurve {
    pub fn curve(&self) -> crate::curve::Curve {
        crate::curve::Curve { scaling: self.scaling, points: self.points.clone() }
    }
}

/// Attenuation curve slots (`curveToUse[7]`).
pub mod atten_slot {
    pub const VOLUME_DRY: usize = 0;
    pub const VOLUME_GAME_AUX: usize = 1;
    pub const VOLUME_USER_AUX: usize = 2;
    pub const LPF: usize = 3;
    pub const HPF: usize = 4;
    pub const SPREAD: usize = 5;
    pub const FOCUS: usize = 6;
}

#[derive(Debug, Clone, Copy)]
pub struct Cone {
    pub inside_deg: f32,
    pub outside_deg: f32,
    pub outside_volume_db: f32,
    pub lpf: f32,
    pub hpf: f32,
}

/// HIRC type 14: u8 coneEnabled, [5 f32 cone], i8 curveToUse[7], u8 nCurves, nCurves × (u8 scaling,
/// u16 n, n × (f32 x, f32 y, u32 interp)), RTPC list. All 2033 attenuations parse exactly. Distances are
/// in game units (the engine passes world positions straight to Wwise; names like amb_med_300_1600 and
/// the table's maxAttenuation 1600 agree).
#[derive(Debug, Clone)]
pub struct Attenuation {
    pub cone: Option<Cone>,
    pub curve_for: [i8; 7],
    pub curves: Vec<crate::curve::Curve>,
    pub rtpcs: Vec<RtpcCurve>,
}

impl Attenuation {
    pub fn curve(&self, slot: usize) -> Option<&crate::curve::Curve> {
        let i = *self.curve_for.get(slot)?;
        if i < 0 { None } else { self.curves.get(i as usize) }
    }

    /// Distance where the dry volume curve ends (Wwise's max attenuation radius).
    pub fn max_distance(&self) -> f32 {
        self.curve(atten_slot::VOLUME_DRY).and_then(|c| c.points.last()).map_or(0.0, |p| p.0)
    }
}

pub fn parse_attenuation(body: &[u8]) -> Result<Attenuation> {
    let mut r = Cursor::new(body);
    let cone = if r.u8()? != 0 {
        Some(Cone { inside_deg: r.f32()?, outside_deg: r.f32()?, outside_volume_db: r.f32()?, lpf: r.f32()?, hpf: r.f32()? })
    } else {
        None
    };
    let mut curve_for = [0i8; 7];
    for c in &mut curve_for {
        *c = r.u8()? as i8;
    }
    let n = r.u8()?;
    let curves = (0..n)
        .map(|_| {
            let scaling = r.u8()?;
            let k = r.u16()? as usize;
            Ok(crate::curve::Curve { scaling, points: points(&mut r, k)? })
        })
        .collect::<Result<_>>()?;
    let rtpcs = rtpcs(&mut r)?;
    ensure!(r.remaining() == 0, "Attenuation has {} unparsed bytes", r.remaining());
    Ok(Attenuation { cone, curve_for, curves, rtpcs })
}
