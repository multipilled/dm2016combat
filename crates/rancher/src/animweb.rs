//! Pose evaluation for the hands anim web. The web itself (states, edges, clocks, events) runs headless in
//! `rancher_sim::animweb` on the game clock; this module evaluates the pose tree it reports like the md6 blend
//! job (ANIMWEB.md section 3): leaves decode the anim at frame + frac (joints the anim lacks keep the bind
//! pose; the pushed weight plane is full), LERP branches run the LERP kernel (0x14173e400) per joint.
//! INTERIM: the md6 key decode at frame + frac uses idres::md6anim's sampler (the engine codec's exact
//! interpolation is not decoded); normalisation uses an exact 1/sqrt instead of rsqrtps + one Newton step.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Quat, Vec3};
use idres::Container;
use idres::md6::Md6Skel;
use idres::md6anim::{Md6Anim, Pose};
use rancher_sim::animweb::{BlendOp, PoseNode};

/// Re-exported for consumers that name it from here (sound.rs).
pub use crate::viewanim::Slot;

/// Parsed animations by md6anim name (lowercase); None when absent from the install.
#[derive(Default)]
pub struct Clips {
    map: HashMap<String, Option<Arc<Md6Anim>>>,
}

impl Clips {
    pub fn load(&mut self, c: &Container, name: &str) -> Option<Arc<Md6Anim>> {
        let key = name.to_ascii_lowercase();
        if let Some(a) = self.map.get(&key) {
            return a.clone();
        }
        let a = c.read_by_name(&idres::animweb::anim_resource(name)).ok().and_then(|b| Md6Anim::parse(&b).map_err(|e| eprintln!("{name}: {e:#}")).ok()).map(Arc::new);
        self.map.insert(key, a.clone());
        a
    }
    pub fn get(&self, name: &str) -> Option<&Arc<Md6Anim>> {
        self.map.get(&name.to_ascii_lowercase()).and_then(|a| a.as_ref())
    }
}

/// Dense local joint transforms for one model.
#[derive(Clone)]
pub struct Locals {
    pub rot: Vec<Quat>,
    pub trans: Vec<Vec3>,
    pub scale: Vec<Vec3>,
}

impl Locals {
    pub fn bind(skel: &Md6Skel) -> Self {
        Locals {
            rot: skel.rotations.iter().map(|r| Quat::from_xyzw(r[0], r[1], r[2], r[3]).normalize()).collect(),
            trans: skel.translations.iter().map(|t| Vec3::from(*t)).collect(),
            scale: skel.scales.iter().map(|s| Vec3::from(*s)).collect(),
        }
    }
    fn overlay(&mut self, p: &Pose) {
        let n = self.rot.len();
        for &(j, q) in &p.rot {
            if (j as usize) < n {
                self.rot[j as usize] = Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize();
            }
        }
        for &(j, v) in &p.trans {
            if (j as usize) < n {
                self.trans[j as usize] = Vec3::from(v);
            }
        }
        for &(j, v) in &p.scale {
            if (j as usize) < n {
                self.scale[j as usize] = Vec3::from(v);
            }
        }
    }
    /// As a pose listing every joint.
    pub fn to_pose(&self) -> Pose {
        let mut p = Pose::default();
        for j in 0..self.rot.len() {
            let q = self.rot[j];
            p.rot.push((j as u16, [q.x, q.y, q.z, q.w]));
            p.trans.push((j as u16, self.trans[j].to_array()));
            p.scale.push((j as u16, self.scale[j].to_array()));
        }
        p
    }
}

/// Evaluates a pose tree into dense local transforms (bind pose for `None`).
pub fn eval(node: Option<&PoseNode>, bind: &Locals, clips: &Clips) -> Locals {
    match node {
        None | Some(PoseNode::Bind) => bind.clone(),
        Some(PoseNode::Leaf { anim, frame, frac }) => match clips.get(anim) {
            Some(clip) => {
                // The decode starts additive / non-0x400 anims from identity, others from the bind pose.
                let mut loc = if clip.base_identity() { identity(bind.rot.len()) } else { bind.clone() };
                loc.overlay(&clip.sample(*frame as f32 + *frac));
                loc
            }
            None => bind.clone(),
        },
        Some(PoseNode::Lerp { left, right, alpha }) => {
            let l = eval(Some(left), bind, clips);
            let r = eval(Some(right), bind, clips);
            lerp_kernel(&l, &r, *alpha)
        }
        Some(PoseNode::Op { op, left, right, alpha }) => {
            let l = eval(Some(left), bind, clips);
            let r = eval(Some(right), bind, clips);
            match op {
                BlendOp::AddLeft => add_kernel(&r, &l, *alpha, false),
                BlendOp::SubLeft => add_kernel(&r, &l, *alpha, true),
                BlendOp::SubRight => add_kernel(&l, &r, *alpha, true),
                // ADD_RIGHT, BLENDA (and LERP-family ops never reach Op).
                _ => add_kernel(&l, &r, *alpha, false),
            }
        }
    }
}

fn identity(n: usize) -> Locals {
    Locals { rot: vec![Quat::IDENTITY; n], trans: vec![Vec3::ZERO; n], scale: vec![Vec3::ONE; n] }
}

/// The LERP kernel with full weight planes on both sides (t = alpha): shortest-arc nlerp in the engine's
/// operation order, linear translation / scale.
fn lerp_kernel(l: &Locals, r: &Locals, t: f32) -> Locals {
    let n = l.rot.len();
    let mut out = l.clone();
    for j in 0..n {
        out.rot[j] = nlerp(l.rot[j], r.rot[j], t);
        out.trans[j] = (r.trans[j] - l.trans[j]) * t + l.trans[j];
        out.scale[j] = (r.scale[j] - l.scale[j]) * t + l.scale[j];
    }
    out
}

/// The kernels' shortest-arc nlerp (sign of t from dot(a, b), operation order of 0x14173e400).
fn nlerp(ql: Quat, qr: Quat, t: f32) -> Quat {
    let d = ql.dot(qr);
    let s = if d < 0.0 { -t } else { t };
    let q = Quat::from_xyzw((ql.x - ql.x * t) + qr.x * s, (ql.y - ql.y * t) + qr.y * s, (ql.z - ql.z * t) + qr.z * s, (ql.w - ql.w * t) + qr.w * s);
    let len2 = q.length_squared();
    if len2 > 0.0 { q * (1.0 / len2.sqrt()) } else { ql }
}

/// ADD kernel 0x141736c90 / SUB kernel 0x14173f650 (ANIMWEB.md 3c) of `delta` onto `base`:
/// t = ((f * c) * Wdelta) * (alpha * c) with c = 1/255; rotation nlerp(qBase, qDelta (x) qBase, t) (SUB: the
/// delta conjugated); scale Sb - (Sb - Sb * Sd) * t (SUB: 1 / Sd); translation Tb + Td * t (SUB: minus).
/// INTERIM: full joint weight planes (filter byte 255, delta weight 255), as in `lerp_kernel`.
fn add_kernel(base: &Locals, delta: &Locals, alpha: f32, sub: bool) -> Locals {
    let c = 1.0f32 / 255.0;
    let t = ((255.0 * c) * 255.0) * (alpha * c);
    let mut out = base.clone();
    for j in 0..base.rot.len() {
        let (b, d) = (base.rot[j], delta.rot[j]);
        let d = if sub { Quat::from_xyzw(-d.x, -d.y, -d.z, d.w) } else { d };
        let p = Quat::from_xyzw(
            d.w * b.x + d.x * b.w + d.y * b.z - d.z * b.y,
            d.w * b.y + d.y * b.w + d.z * b.x - d.x * b.z,
            d.w * b.z + d.z * b.w + d.x * b.y - d.y * b.x,
            d.w * b.w - d.x * b.x - d.y * b.y - d.z * b.z,
        );
        out.rot[j] = nlerp(b, p, t);
        let (sb, sd) = (base.scale[j], delta.scale[j]);
        let sd = if sub { Vec3::ONE / sd } else { sd };
        out.scale[j] = sb - (sb - sb * sd) * t;
        out.trans[j] = if sub { base.trans[j] - delta.trans[j] * t } else { base.trans[j] + delta.trans[j] * t };
    }
    out
}
