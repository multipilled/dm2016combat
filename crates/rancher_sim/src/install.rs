//! Builds [`MoveConfig`] from the user's DOOM (2016) install: exe cvar defaults, then the shipped
//! `default.cfg` and `default_sp.cfg` (single-player resets/overrides), then the jump-boots decl.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::md6::Md6Skel;
use idres::md6anim::Md6Anim;
use idres::md6def::Md6DefDecl;
use idres::{Container, crypt, decldb::DeclDb, exe};

use crate::config::{CvarValues, DoubleJump, LedgeAnim, MoveConfig, PlayerFalling};

pub struct Install {
    pub cvars: CvarValues,
    pub decls: DeclDb,
    pub movement: MoveConfig,
}

/// The jump boots the single-player campaign equips.
pub const SP_JUMP_BOOTS: &str = "jumpboots/base";

pub fn load(doom: &Path) -> Result<Install> {
    let table = exe::scan_cvars(&doom.join("DOOMx64.exe"))?;
    let mut values: HashMap<String, String> = table.cvars.iter().map(|(k, v)| (k.clone(), v.default.clone())).collect();
    let container = Arc::new(Container::open(&doom.join("base"), "gameresources")?);
    for cfg in ["generated/binaryfile/default.bfile", "generated/binaryfile/default_sp.bfile"] {
        let entry = container.get(cfg).with_context(|| format!("{cfg} not found"))?;
        let bytes = container.read(entry)?;
        let text = crypt::decrypt(&bytes, &entry.short_name).with_context(|| format!("decrypting {cfg}"))?;
        exe::apply_cfg(&mut values, &table, &String::from_utf8_lossy(&text));
    }
    let cvars = CvarValues(values);
    let decls = DeclDb::new(container);

    // Fields the decl omits keep the decl class defaults, which equal these cvar defaults.
    let boots = decls.get("jumpboots", SP_JUMP_BOOTS)?;
    let edit = boots.block("edit").context("jump boots decl has no edit block")?;
    let f = |key: &str, cvar: &str| edit.f32(key).unwrap_or_else(|| cvars.f(cvar));
    let dj = DoubleJump {
        height: f("doubleJumpHeight", "pm_doubleJumpHeight"),
        speed_scale: f("doubleJumpSpeedScale", "pm_doubleJumpSpeedScale"),
        air_english_scale: f("doubleJumpAirEnglishScale", "pm_doubleJumpAirEnglishScale"),
        window_offset_ms: f("doubleJumpWindowOffset", "pm_doubleJumpWindowOffset") as i32,
        window_duration_ms: f("doubleJumpWindowDuration", "pm_doubleJumpWindowDuration") as i32,
    };
    let ledge_anims = ledge_grab_anims(&decls).context("loading ledge grab animations")?;
    let mut movement = MoveConfig::from_cvars(&cvars, Some(dj), ledge_anims);
    // idDeclPlayerProps (player decl `playerProps`): playerFalling over the class defaults.
    let player = decls.get("entitydef", "player")?;
    if let Some(props) = player.str("edit.playerProps") {
        let props = decls.get("playerprops", props)?;
        movement.falling = PlayerFalling::with_overrides(|k| props.f32(&format!("edit.playerFalling.{k}")));
    }
    Ok(Install { cvars, decls, movement })
}

/// animAliases keys of playerMechanicLedgeGrab in idPlayerMechanicLedgeGrabState_t order (0x140d9f750).
const LEDGE_STATE_ALIASES: [&str; 16] = [
    "ledgePullUp",
    "ledgePullUpMantle",
    "ledgePullUpFoot",
    "ledgePullUpRounded",
    "ledgePullUpMantleRounded",
    "ledgePullUpAngled",
    "ledgePullUpMantleAngled",
    "ledgeClimbUp",
    "ledgeClimbUpMantle",
    "ledgeClimbUpFoot",
    "railingPullUp",
    "railingPullUpMantle",
    "railingPullUpFoot",
    "customLedgeGrabPullUp",
    "customLedgeGrabMantle",
    "customLedgeGrabFoot",
];

/// The precache of 0x140d986c0: each state's alias resolved through the third-person body model
/// (player decl thirdPersonBodyDefault → its md6 decl's aliases), then the "origin" joint at the last
/// frame and the "align" joint at frame 0, in model space. States without an alias stay zero.
fn ledge_grab_anims(decls: &DeclDb) -> Result<[LedgeAnim; 16]> {
    let mut out: [LedgeAnim; 16] = Default::default();
    let player = decls.get("entitydef", "player")?;
    let Some(aliases) = player.block("edit.playerMechanicLedgeGrab.animAliases") else { return Ok(out) };
    let body = player.str("edit.thirdPersonBodyDefault").context("player has no thirdPersonBodyDefault")?;
    let body = decls.get("entitydef", body)?;
    let model = body.str("edit.renderModelInfo.model").context("third-person body has no model")?;
    let md6 = md6_def(decls.container(), model, 0)?;
    let container = decls.container();
    for (state, key) in LEDGE_STATE_ALIASES.iter().enumerate() {
        let Some(alias) = aliases.str(key).filter(|a| !a.is_empty()) else { continue };
        let Some(path) = md6.aliases.get(alias) else { continue };
        let anim = Md6Anim::parse(&container.read_by_name(&idres::animweb::anim_resource(path))?)?;
        let skel_res = format!("generated/skeleton/{}.bmd6skl", anim.skeleton.trim_end_matches(".md6skl")).to_ascii_lowercase();
        let skel = Md6Skel::parse(&container.read_by_name(&skel_res)?)?;
        let joint = |name: &str| skel.names.iter().position(|n| n == name).with_context(|| format!("{skel_res} has no {name} joint"));
        let last = anim.num_frames.saturating_sub(1) as f32;
        let camera = joint("camera")?;
        out[state] = LedgeAnim {
            origin_destination: joint_model_pos(&anim, &skel, joint("origin")?, last),
            align_pos: joint_model_pos(&anim, &skel, joint("align")?, 0.0),
            num_frames: anim.num_frames,
            frame_rate: anim.frame_rate,
            camera: (0..anim.num_frames).map(|f| joint_model_pos(&anim, &skel, camera, f as f32)).collect(),
        };
    }
    Ok(out)
}

fn md6_def(container: &Container, name: &str, depth: u32) -> Result<Md6DefDecl> {
    let path = format!("generated/decls/md6def/{name}.decl");
    let text = String::from_utf8_lossy(&container.read_by_name(&path)?).into_owned();
    let own = Md6DefDecl::parse(&text).with_context(|| format!("parsing {path}"))?;
    match own.inherit.as_deref() {
        Some(parent) if depth < 16 => Ok(md6_def(container, parent, depth + 1)?.merged_with(&own)),
        _ => Ok(own),
    }
}

/// Model-space position of `joint` at `frame`: the anim's local transforms (bind pose where the anim
/// has no channel) composed down the hierarchy.
fn joint_model_pos(anim: &Md6Anim, skel: &Md6Skel, joint: usize, frame: f32) -> Vec3 {
    let pose = anim.sample(frame);
    let mut chain = vec![joint];
    while let Some(&j) = chain.last() {
        match usize::try_from(skel.parents[j]) {
            Ok(p) => chain.push(p),
            Err(_) => break,
        }
    }
    let mut pos = Vec3::ZERO;
    let mut rot = glam::Quat::IDENTITY;
    for &j in chain.iter().rev() {
        let t = pose.trans.iter().rev().find(|p| p.0 as usize == j).map(|p| p.1).unwrap_or(skel.translations[j]);
        let r = pose.rot.iter().rev().find(|p| p.0 as usize == j).map(|p| p.1).unwrap_or(skel.rotations[j]);
        pos += rot * Vec3::from(t);
        rot *= glam::Quat::from_xyzw(r[0], r[1], r[2], r[3]);
    }
    pos
}
