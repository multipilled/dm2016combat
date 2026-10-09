//! Where the HUD movies sit on screen, as the game computes it (notes in gamedata/re/SWF.md).
//!
//! Each HUD component renders its movie onto a gui quad (`models/guis/gui_square_hud.lwo`: x = 0,
//! y and z in -12..12, gui (0,0) at y = -12, z = 12, s along +y, t along -z) whose render entity is scaled
//! (1, scale * aspect, scale) with aspect = frameW / frameH (idMenuManager update 0x140f91230). The quad is
//! placed in view space on a tag of the player's helmet model (`marine_helmet.md6`, prop "_info": translation
//! and rotation relative to its static `origin` joint) by 0x140be1360: origin = view origin + view axis * tag
//! translation, axis = tag rotation * view axis, then lifted 0.05 along its own up axis (0x140be1320). The
//! vitals and weapon-info tags get an extra yaw (`view_hudSkewModifiersEnabled`; 0x140c2fa60, 0x140c32620).
//! Panels are projected with their own fixed 80-degree field of view (CalcFov 0x140e6bc00, set on the
//! component's render entity at the end of 0x140be1360), independent of g_fov.
//!
//! The reticle (0x140c307d0) sits on the `reticle_display` tag without the lift; with the default
//! `view_UseBrokenReticleSWFScaling 1` its axis is turned half a turn about its up axis (the movie is seen
//! mirrored) and its scale is the weaponReticle decl's `reticleModelScale`. It keeps the view's projection.
//!
//! View space here is the game's: x forward, y left, z up.

use std::collections::HashMap;

use anyhow::{Context, Result};

/// Field of view of the HUD panels (0x140be18b3; the helmet uses the same, 0x140dc890e).
pub const HUD_FOV: f32 = 80.0;
/// Lift of every tag-placed panel along its up axis (0x140be1320).
pub const PANEL_LIFT: f32 = 0.05;
/// `view_hudSkewModifiersEnabled`, `view_hudSkewLeftModifier`, `view_hudSkewRightModifier` (radians).
pub const SKEW_ENABLED: bool = true;
pub const SKEW_LEFT: f32 = 0.24;
pub const SKEW_RIGHT: f32 = -0.32;
/// Below this aspect ratio the skewed panels move in (offsets (0, 0.8, 0.05) / (0, -0.9, 0.05)).
pub const NARROW_ASPECT: f32 = 1.4;

/// Component scale cvars (`hud_playerInfoScale`, `hud_weaponInfoScale`, `hud_bottomScale`, `hud_topLeftScale`).
pub const PLAYER_INFO_SCALE: f32 = 0.1;
pub const WEAPON_INFO_SCALE: f32 = 0.083;
pub const BOTTOM_SCALE: f32 = 0.1;
pub const TOP_LEFT_SCALE: f32 = 0.085;
/// idDeclWeaponReticle `reticleModelScale` class default (ctor 0x1406f1840; every SP reticle decl sets it).
pub const DEFAULT_RETICLE_SCALE: f32 = 1.0;

/// The helmet model whose "_info" prop holds the HUD tags.
pub const HELMET_MD6: &str = "generated/decls/md6def/zion/player/human/base/marine_helmet.md6.decl";

/// A tag: translation and rotation (quaternion x, y, z, w) relative to its parent joint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tag {
    pub trans: [f32; 3],
    pub rot: [f32; 4],
}

/// Tags of one md6 prop, by name, from md6Def decl text (`prop "<prop>" { tag "<name>" { trans ( x y z ) rot ( x y z w ) parent "<joint>" } }`).
pub fn parse_tags(text: &str, prop: &str) -> HashMap<String, Tag> {
    let toks: Vec<&str> = text.split(|c: char| c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    let quoted = |t: &str| t.trim_matches('"').to_string();
    let mut out = HashMap::new();
    let mut i = 0;
    while i + 1 < toks.len() {
        if toks[i] == "prop" && quoted(toks[i + 1]) == prop {
            // Walk the prop's block.
            let mut depth = 0;
            let mut j = i + 2;
            let mut name = None;
            let mut trans = [0.0; 3];
            let mut rot = [0.0, 0.0, 0.0, 1.0];
            while j < toks.len() {
                match toks[j] {
                    "{" => depth += 1,
                    "}" => {
                        depth -= 1;
                        if depth == 1 {
                            if let Some(n) = name.take() {
                                out.insert(n, Tag { trans, rot });
                            }
                        }
                        if depth == 0 {
                            break;
                        }
                    }
                    "tag" if j + 1 < toks.len() => {
                        name = Some(quoted(toks[j + 1]));
                        trans = [0.0; 3];
                        rot = [0.0, 0.0, 0.0, 1.0];
                    }
                    "trans" if j + 5 < toks.len() => {
                        for k in 0..3 {
                            trans[k] = toks[j + 2 + k].parse().unwrap_or(0.0);
                        }
                    }
                    "rot" if j + 6 < toks.len() => {
                        for k in 0..4 {
                            rot[k] = toks[j + 2 + k].parse().unwrap_or(0.0);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
    out
}

/// The HUD tags of the player's helmet ("_info" prop of marine_helmet.md6).
pub fn load_helmet_tags(container: &idres::Container) -> Result<HashMap<String, Tag>> {
    let b = container.read_by_name(HELMET_MD6).with_context(|| HELMET_MD6.to_string())?;
    let tags = parse_tags(&String::from_utf8_lossy(&b), "_info");
    anyhow::ensure!(tags.contains_key("player_info"), "no HUD tags in {HELMET_MD6}");
    Ok(tags)
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx, aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz]
}

/// idQuat::ToMat3: rows are the rotated x, y, z axes.
fn quat_mat(q: [f32; 4]) -> [[f32; 3]; 3] {
    let [x, y, z, w] = q;
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [[1.0 - (yy + zz), xy + wz, xz - wy], [xy - wz, 1.0 - (xx + zz), yz + wx], [xz + wy, yz - wx, 1.0 - (xx + yy)]]
}

/// A movie's quad in view space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Panel {
    /// Quad centre (game units, view space).
    pub origin: [f32; 3],
    /// Rows: quad normal (model x), model y (the gui's s runs along +y), model z (up; t runs along -z).
    pub axis: [[f32; 3]; 3],
    /// Component scale.
    pub scale: f32,
}

impl Panel {
    /// Places a panel on `tag`: optional yaw `skew` (radians, applied in the tag's frame), `offset` along the
    /// resulting axes (0x140be1360).
    pub fn on_tag(tag: &Tag, skew: f32, offset: [f32; 3], scale: f32) -> Panel {
        let rot = if skew != 0.0 { quat_mul(tag.rot, [0.0, 0.0, (skew * 0.5).sin(), (skew * 0.5).cos()]) } else { tag.rot };
        let axis = quat_mat(rot);
        let mut origin = tag.trans;
        for (k, o) in offset.iter().enumerate() {
            for c in 0..3 {
                origin[c] += o * axis[k][c];
            }
        }
        Panel { origin, axis, scale }
    }

    /// A panel on `tag` turned by idAngles `angles` (pitch, yaw, roll in degrees) in the tag's frame, without the
    /// lift (0x140f8ef50, used by the weapon wheel). INTERIM: yaw is applied before pitch (the exact product order of
    /// idAngles::ToMat3 0x1402d90c0 with the tag axis is not checked; the wheel's angles are half a degree).
    pub fn on_tag_angles(tag: &Tag, angles: [f32; 3], scale: f32) -> Panel {
        let half = |deg: f32| (deg.to_radians() * 0.5).sin_cos();
        let (sp, cp) = half(angles[0]);
        let (sy, cy) = half(angles[1]);
        let (sr, cr) = half(angles[2]);
        let q = quat_mul(quat_mul([0.0, 0.0, sy, cy], [0.0, sp, 0.0, cp]), [sr, 0.0, 0.0, cr]);
        Panel { origin: tag.trans, axis: quat_mat(quat_mul(tag.rot, q)), scale }
    }

    /// The reticle's placement (0x140c307d0): the tag without lift, turned half a turn about its up axis.
    pub fn reticle(tag: &Tag, scale: f32) -> Panel {
        let mut p = Panel::on_tag(tag, 0.0, [0.0; 3], scale);
        for c in 0..3 {
            p.axis[0][c] = -p.axis[0][c];
            p.axis[1][c] = -p.axis[1][c];
        }
        p
    }

    /// View-space position of stage point `p` of a `frame_w` x `frame_h` movie drawn on this panel.
    pub fn stage_to_view(&self, p: [f32; 2], frame_w: f32, frame_h: f32) -> [f32; 3] {
        let aspect = frame_w / frame_h;
        let ly = (-12.0 + 24.0 * p[0] / frame_w) * self.scale * aspect;
        let lz = (12.0 - 24.0 * p[1] / frame_h) * self.scale;
        std::array::from_fn(|c| self.origin[c] + ly * self.axis[1][c] + lz * self.axis[2][c])
    }
}

/// CalcFov (0x140e6bc00): `fov` is the horizontal field of view at 16:9; returns (fov_x, fov_y) in degrees
/// for a `width` x `height` view (aspect clamped to 1..3).
pub fn calc_fov(fov: f32, width: f32, height: f32) -> (f32, f32) {
    let fov = if fov < 1.0 { 90.0 } else { fov };
    let fy = ((fov * 0.017453292 * 0.5).tan() * 0.5625).atan();
    let fov_y = fy * 57.295776 + fy * 57.295776;
    let aspect = (width / height).clamp(1.0, 3.0);
    let fx = ((fov_y * 0.017453292 * 0.5).tan() * aspect).atan();
    (fx * 57.295776 + fx * 57.295776, fov_y)
}

/// A perspective projection from the eye (view space, x forward).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    pub tan_x: f32,
    pub tan_y: f32,
}

impl Projection {
    pub fn from_fov(fov: f32, width: f32, height: f32) -> Projection {
        let (fx, fy) = calc_fov(fov, width, height);
        Projection { tan_x: (fx.to_radians() * 0.5).tan(), tan_y: (fy.to_radians() * 0.5).tan() }
    }

    /// Normalised device coordinates (x right, y up) and depth.
    pub fn ndc(&self, v: [f32; 3]) -> [f32; 3] {
        [-v[1] / v[0] / self.tan_x, v[2] / v[0] / self.tan_y, v[0]]
    }

    /// Window pixels (y down) for a `width` x `height` window.
    pub fn to_screen(&self, v: [f32; 3], width: f32, height: f32) -> [f32; 2] {
        let n = self.ndc(v);
        [(n[0] + 1.0) * 0.5 * width, (1.0 - n[1]) * 0.5 * height]
    }
}

/// The SP HUD movies' panels and projections for a `width` x `height` window. `view_fov` is the view's g_fov
/// style field of view (for the reticle); `reticle_scale` the current weaponReticle `reticleModelScale`.
pub struct HudLayout {
    pub panels: Vec<(&'static str, Panel, Projection)>,
}

impl HudLayout {
    pub fn new(tags: &HashMap<String, Tag>, width: f32, height: f32, view_fov: f32, reticle_scale: f32) -> HudLayout {
        let hud = Projection::from_fov(HUD_FOV, width, height);
        let view = Projection::from_fov(view_fov, width, height);
        let narrow = width / height < NARROW_ASPECT;
        let mut panels = Vec::new();
        let mut add = |name: &'static str, tag: &str, skew: f32, shift: f32, scale: f32| {
            if let Some(t) = tags.get(tag) {
                let (skew, off) = if SKEW_ENABLED { (skew, [0.0, if narrow { shift } else { 0.0 }, PANEL_LIFT]) } else { (0.0, [0.0, 0.0, PANEL_LIFT]) };
                panels.push((name, Panel::on_tag(t, skew, off, scale), hud));
            }
        };
        add("hud_bottom_left", "player_info", SKEW_LEFT, 0.8, PLAYER_INFO_SCALE);
        add("ws_0", "weaponinfo", SKEW_RIGHT, -0.9, WEAPON_INFO_SCALE);
        add("hud_bottom", "bottom_hud", 0.0, 0.0, BOTTOM_SCALE);
        add("hud_top_left", "top_left_hud", 0.0, 0.0, TOP_LEFT_SCALE);
        if let Some(t) = tags.get("reticle_display") {
            panels.push(("reticle", Panel::reticle(t, reticle_scale), view));
        }
        HudLayout { panels }
    }

    pub fn get(&self, name: &str) -> Option<(Panel, Projection)> {
        self.panels.iter().find(|(n, ..)| *n == name).map(|(_, p, j)| (*p, *j))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_fov_matches_16_9() {
        let (fx, fy) = calc_fov(90.0, 1920.0, 1080.0);
        assert!((fx - 90.0).abs() < 1e-3, "{fx}");
        assert!((fy - 58.7155).abs() < 1e-3, "{fy}");
        let (fx, _) = calc_fov(90.0, 1024.0, 768.0);
        assert!((fx - 73.74).abs() < 0.01, "{fx}");
    }

    #[test]
    fn tags_parse_and_face_the_eye() {
        let text = "props { prop \"_info\" { tag \"bottom_hud\" { trans ( 7.3549 0 -2.5308 ) rot ( 0 0 -1 0 ) parent \"origin\" } } prop \"x\" { tag \"bottom_hud\" { trans ( 1 2 3 ) rot ( 0 0 0 1 ) } } }";
        let tags = parse_tags(text, "_info");
        let t = tags["bottom_hud"];
        assert_eq!(t.trans, [7.3549, 0.0, -2.5308]);
        let p = Panel::on_tag(&t, 0.0, [0.0, 0.0, PANEL_LIFT], 0.1);
        // Normal points back at the eye, the gui's s axis runs to the viewer's right (-y).
        assert!(p.axis[0][0] < -0.99 && p.axis[1][1] < -0.99);
        assert!((p.origin[2] - (-2.4808)).abs() < 1e-5);
        let left = p.stage_to_view([0.0, 150.0], 512.0, 300.0);
        assert!(left[1] > 0.0, "stage x = 0 is on the viewer's left");
    }
}
