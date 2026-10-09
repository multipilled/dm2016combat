//! idHands' additive channel drivers (DOOMx64.exe build 13954591; notes in gamedata/re/HANDSLAYERS.md section 6):
//! the looping-shoot additive (additiveShootAnimator, hands +0x5868, 0x140d64410) and the additive offset
//! (additiveOffsetAnimator, hands +0x5988, 0x140d7b850). Both are idManagedChannelAnimators merged onto the hands pose
//! with BOP_ADD_RIGHT (layer parms 0x142043360: op 4; maxMergeAlpha 1.0). The drivers decide WHAT to play; the
//! renderer runs the channel ([`ChannelCmd`] semantics below) and merges its additive pose (ANIMWEB.md 3c).

use idres::decl::Block;

/// What a channel is told to do this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelCmd {
    /// Play the hands md6Def alias from frame 0 at `rate` (0x14066afa0 -> 0x1416f6d20 -> 0x1416f6ff0): one of the
    /// alias's anims picked at random (channel LCG), a crossfade from the channel's previous anim over
    /// `blend_frames` 30 Hz frames (blend type `blend_type`), and the merge alpha fading linearly from its current
    /// value (0 when the channel was empty) to 1.0 over the same time (snap when 0 frames). Then, when `alpha` is
    /// Some, the merge alpha is snapped to it (0x14066b4d0 with rate 0).
    Play { alias: String, rate: f32, blend_frames: i32, blend_type: u8, alpha: Option<f32> },
    /// Stop (0x14066b830 -> 0x1416f5350): merge target alpha 0, fading linearly from the current alpha over `ms`
    /// (at once when it is under one tick), then the channel's anims are dropped.
    Stop { ms: i32 },
}

/// Blend parms from ms (0x1416e65b0): destDuration = (int)(ms * 30 / 1000) frames, clamped to s16.
pub fn blend_frames(ms: i32) -> i32 {
    ((ms as f32 * 30.0 / 1000.0) as i32).clamp(-0x7fff, 0x7fff)
}

/// The blend time in ms a destDuration gives (0x1416e5da0).
pub fn blend_ms(frames: i32) -> i32 {
    ((frames as f32 * 1000.0) / 30.0) as i32
}

/// The idDeclWeapon fields both drivers read (ctor 0x1406f0780 defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct AdditiveDecl {
    /// additiveLoopingShootAnim (+0x9c8), Alpha (+0x9d0, 1.0), Cycles (+0x9d4, 1).
    pub looping_shoot: String,
    pub looping_shoot_alpha: f32,
    pub looping_shoot_cycles: i32,
    /// additiveLoopingShootAnimZoomed (+0x9d8), ZoomedAlpha (+0x9e0, 1.0), ZoomedCycles (+0x9e4, 1).
    pub looping_shoot_zoomed: String,
    pub looping_shoot_zoomed_alpha: f32,
    pub looping_shoot_zoomed_cycles: i32,
    /// additiveOffsetAnim (+0xbe8), ZoomAnim (+0xbf0), MeleeAnim (+0xbf8), MeleeAnimBlendMS (+0xc00, -1),
    /// EmptyAnim (+0xc08).
    pub offset: String,
    pub offset_zoom: String,
    pub offset_melee: String,
    pub offset_melee_blend_ms: i32,
    pub offset_empty: String,
    /// ironSightZoom.zoomBlendTime (+0xe08 via 0x140f14da0; a mod's own decl wins).
    pub zoom_blend_time_ms: i32,
}

impl Default for AdditiveDecl {
    fn default() -> Self {
        Self {
            looping_shoot: String::new(),
            looping_shoot_alpha: 1.0,
            looping_shoot_cycles: 1,
            looping_shoot_zoomed: String::new(),
            looping_shoot_zoomed_alpha: 1.0,
            looping_shoot_zoomed_cycles: 1,
            offset: String::new(),
            offset_zoom: String::new(),
            offset_melee: String::new(),
            offset_melee_blend_ms: -1,
            offset_empty: String::new(),
            zoom_blend_time_ms: 0,
        }
    }
}

impl AdditiveDecl {
    /// From a weapon decl's `edit` block.
    pub fn from_edit(e: &Block) -> Self {
        let d = Self::default();
        let s = |k: &str| e.str(k).unwrap_or_default().to_string();
        Self {
            looping_shoot: s("additiveLoopingShootAnim"),
            looping_shoot_alpha: e.f32("additiveLoopingShootAnimAlpha").unwrap_or(d.looping_shoot_alpha),
            looping_shoot_cycles: e.f32("additiveLoopingShootAnimCycles").map(|v| v as i32).unwrap_or(d.looping_shoot_cycles),
            looping_shoot_zoomed: s("additiveLoopingShootAnimZoomed"),
            looping_shoot_zoomed_alpha: e.f32("additiveLoopingShootAnimZoomedAlpha").unwrap_or(d.looping_shoot_zoomed_alpha),
            looping_shoot_zoomed_cycles: e
                .f32("additiveLoopingShootAnimZoomedCycles")
                .map(|v| v as i32)
                .unwrap_or(d.looping_shoot_zoomed_cycles),
            offset: s("additiveOffsetAnim"),
            offset_zoom: s("additiveOffsetZoomAnim"),
            offset_melee: s("additiveOffsetMeleeAnim"),
            offset_melee_blend_ms: e.f32("additiveOffsetMeleeAnimBlendMS").map(|v| v as i32).unwrap_or(d.offset_melee_blend_ms),
            offset_empty: s("additiveOffsetEmptyAnim"),
            zoom_blend_time_ms: e.f32("ironSightZoom.zoomBlendTime").map(|v| v as i32).unwrap_or(0),
        }
    }
}

/// additiveLoopingShootAnimRate / ...ZoomedRate (hands +0x10350 / +0x10354, set at weapon init 0x140d62b24):
/// the alias's (numFrames - 1) / frameRate seconds spread over `cycles` firing intervals of game time (960 ticks/s).
pub fn looping_shoot_rate(num_frames: u32, frame_rate: u32, cycles: i32, firing_interval: i32) -> f32 {
    let len = (num_frames as i32 - 1) as f32 / frame_rate as f32;
    len * 960.0 / ((cycles as f32) * (firing_interval.max(1) as f32))
}

/// Inputs of the looping-shoot driver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopingShootInput {
    /// hands_additiveShootAnimEnable (default 1).
    pub enable: bool,
    /// destHandsState (hands +0x3b28) is LOOPING_SHOOT_STATE (8) or CHARGE_LOOPING_SHOOT (11): the driver is skipped.
    pub dest_looping: bool,
    /// handsFlags byte 1 (hands +0x10375): firing = (byte & 0x12) == 0x12.
    pub hands_flags1: u8,
    /// The player is zoomed (0x140e42290).
    pub zoomed: bool,
    /// The channel is done (0x1416f6860: no anim, or a CLAMP anim past its end).
    pub channel_done: bool,
    /// Which of the two aliases the channel is playing right now (0x1416f65f0 per alias).
    pub playing_normal: bool,
    pub playing_zoomed: bool,
    /// hands_additiveAnimBlendMS (150).
    pub blend_ms: i32,
    /// The two rates (see [`looping_shoot_rate`]).
    pub rate: f32,
    pub rate_zoomed: f32,
}

/// Per-frame call from UpdateWeapon (0x140d7ec11) into 0x140d64410.
pub fn looping_shoot(d: &AdditiveDecl, inp: &LoopingShootInput) -> Option<ChannelCmd> {
    if inp.dest_looping {
        return None;
    }
    let firing = inp.hands_flags1 & 0x12 == 0x12;
    if !inp.enable || !firing {
        return (!inp.channel_done).then_some(ChannelCmd::Stop { ms: 0 });
    }
    // Both aliases must resolve on the hands model, or nothing is played.
    if d.looping_shoot.is_empty() || d.looping_shoot_zoomed.is_empty() {
        return None;
    }
    let (alias, alpha, rate) = if !inp.zoomed {
        if inp.playing_normal {
            return None;
        }
        (&d.looping_shoot, d.looping_shoot_alpha, inp.rate)
    } else {
        if inp.playing_zoomed {
            return None;
        }
        (&d.looping_shoot_zoomed, d.looping_shoot_zoomed_alpha, inp.rate_zoomed)
    };
    Some(ChannelCmd::Play { alias: alias.clone(), rate, blend_frames: blend_frames(inp.blend_ms), blend_type: 0, alpha: Some(alpha) })
}

/// idHands::additiveOffsetAnimType_t (hands +0x5b7c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OffsetType {
    #[default]
    None = 0,
    Default = 1,
    Zoom = 2,
    Melee = 3,
    Empty = 4,
}

/// Inputs of the additive offset driver.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OffsetInput {
    /// The current weapon is held in the right hand (slot +0x1b4 == 2); otherwise nothing happens.
    pub right_hand: bool,
    /// handsFlags +0x10378 bit 0, or 0x140d63390(hands): no offset.
    pub suppressed: bool,
    /// handsFlags +0x10376 & 6 (melee / throw).
    pub melee: bool,
    /// The player is zoomed (0x140e42290).
    pub zoomed: bool,
    /// A weapon mod's own zoom offset anim (weapon + mode*400 + 0x19c0), when set.
    pub mod_zoom_anim: Option<String>,
    /// 0x140f03930(weapon) == 0 (empty clip).
    pub empty: bool,
    /// hands_additiveOffsetBlendDurationMS (-1).
    pub blend_duration_cvar: i32,
    /// The channel's current alias, if any (curAliasHandle).
    pub current_alias: Option<String>,
    /// Re-apply even when the type is unchanged (the call's second argument).
    pub force: bool,
}

/// UpdateAdditiveOffset (0x140d7b850). `state` is hands +0x5b7c, updated in place.
pub fn additive_offset(d: &AdditiveDecl, inp: &OffsetInput, state: &mut OffsetType) -> Option<ChannelCmd> {
    if !inp.right_hand {
        return None;
    }
    let prev = *state;
    let zoom = inp.mod_zoom_anim.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| d.offset_zoom.clone());
    let (t, anim) = if inp.suppressed {
        (OffsetType::None, String::new())
    } else if inp.melee && !d.offset_melee.is_empty() {
        (OffsetType::Melee, d.offset_melee.clone())
    } else if inp.zoomed && !zoom.is_empty() {
        (OffsetType::Zoom, zoom)
    } else if inp.empty && !d.offset_empty.is_empty() {
        (OffsetType::Empty, d.offset_empty.clone())
    } else if !d.offset.is_empty() {
        (OffsetType::Default, d.offset.clone())
    } else {
        (OffsetType::None, String::new())
    };
    *state = t;
    if t == prev && !inp.force {
        return None;
    }
    let either = |x: OffsetType| prev == x || t == x;
    let (frames, blend_type) = if inp.blend_duration_cvar >= 0 {
        (blend_frames(inp.blend_duration_cvar), 0)
    } else if either(OffsetType::Melee) {
        (blend_frames(if d.offset_melee_blend_ms >= 0 { d.offset_melee_blend_ms } else { 3 }), 0)
    } else if either(OffsetType::Zoom) {
        (blend_frames(d.zoom_blend_time_ms), 0)
    } else if either(OffsetType::Empty) {
        // 200 ms with blendType EASEIN_EASEOUT.
        (blend_frames(200), 3)
    } else {
        (blend_frames(3), 0)
    };
    if anim.is_empty() {
        return Some(ChannelCmd::Stop { ms: blend_ms(frames) });
    }
    if inp.current_alias.as_deref() == Some(anim.as_str()) {
        return None;
    }
    Some(ChannelCmd::Play { alias: anim, rate: 1.0, blend_frames: frames, blend_type, alpha: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looping_shoot_plays_while_firing() {
        let d = AdditiveDecl {
            looping_shoot: "assaultrifles/base/shoot_additive".into(),
            looping_shoot_zoomed: "assaultrifles/assaultrifle/zoomshoot_additive".into(),
            looping_shoot_zoomed_alpha: 0.1,
            ..Default::default()
        };
        let mut inp = LoopingShootInput {
            enable: true,
            dest_looping: false,
            hands_flags1: 0x12,
            zoomed: false,
            channel_done: true,
            playing_normal: false,
            playing_zoomed: false,
            blend_ms: 150,
            rate: 1.5,
            rate_zoomed: 1.0,
        };
        assert_eq!(
            looping_shoot(&d, &inp),
            Some(ChannelCmd::Play { alias: d.looping_shoot.clone(), rate: 1.5, blend_frames: 4, blend_type: 0, alpha: Some(1.0) })
        );
        inp.playing_normal = true;
        assert_eq!(looping_shoot(&d, &inp), None);
        inp.zoomed = true;
        assert!(matches!(looping_shoot(&d, &inp), Some(ChannelCmd::Play { alpha: Some(a), .. }) if a == 0.1));
        inp.hands_flags1 = 0x10;
        inp.channel_done = false;
        assert_eq!(looping_shoot(&d, &inp), Some(ChannelCmd::Stop { ms: 0 }));
        inp.dest_looping = true;
        assert_eq!(looping_shoot(&d, &inp), None);
        assert!((looping_shoot_rate(31, 30, 6, 64) - 1.0 * 960.0 / 384.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_offset_plays_and_stops() {
        let d = AdditiveDecl { offset_zoom: "gauss_rifle/additive_reposition_zoomed".into(), zoom_blend_time_ms: 150, ..Default::default() };
        let mut st = OffsetType::None;
        let inp = OffsetInput { right_hand: true, zoomed: true, blend_duration_cvar: -1, ..Default::default() };
        assert_eq!(
            additive_offset(&d, &inp, &mut st),
            Some(ChannelCmd::Play { alias: d.offset_zoom.clone(), rate: 1.0, blend_frames: 4, blend_type: 0, alpha: None })
        );
        assert_eq!(st, OffsetType::Zoom);
        assert_eq!(additive_offset(&d, &inp, &mut st), None);
        let out = OffsetInput { zoomed: false, current_alias: Some(d.offset_zoom.clone()), ..inp };
        assert_eq!(additive_offset(&d, &out, &mut st), Some(ChannelCmd::Stop { ms: 133 }));
        assert_eq!(st, OffsetType::None);
    }
}
