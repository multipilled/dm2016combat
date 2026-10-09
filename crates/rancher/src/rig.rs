//! idTech 6 rig post-processing for the first-person arms, ported from the user's own DOOMx64.exe.
//!
//! The arms skeleton is "reversed": each hand hangs off its `*handattach` joint and the forearm-roll /
//! forearm / upper-arm chain hangs off the hand. Hands animations do not key the hand joints; they key
//! IK handles (`rig_arm_*_root` = shoulder, `rig_arm_*_target` = hand, `rig_arm_*_pole` = elbow swivel
//! as a local X rotation). Every frame the game runs, per arm (rig builder 0x14171a820):
//!  1. `idRigIk2Segments` (solver 0x141a451f0 / 0x141a428b0 / 0x141a41ec0) on the chain
//!     rig root -> rig hinge -> hand, toward the target, swivelled by the pole (`rig_skipIk2Pole` 0).
//!  2. Copy constraints (0x141a44910): the forearm-roll joints follow the hinge with their bind offsets,
//!     the forearm takes the hinge transform and the upper arm takes the rig root transform.
//!  3. Twist distribution (0x141a45920, `rig_skipTwist` 0) onto the roll joints with weights
//!     1.0 / 0.75 / 0.5 / 0.25, keeping their children fixed.
//! The descriptors are built from the bind pose after copying arm -> rig root and forearm -> rig hinge
//! (0x14171cf10), as the game does.
//!
//! Model-space writes keep the written joint's descendants attached through their local transforms.

use glam::{Mat4, Quat, Vec3};
use idres::md6::Md6Skel;
use idres::md6anim::Pose;

/// Smallest normal float, the engine's clamp before reciprocal square roots.
const TINY: f32 = 1.175_494_4e-38;
const EPS: f32 = 1.192_092_9e-7;

#[derive(Clone, Copy, Debug)]
pub struct Xf {
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
}

impl Xf {
    fn child(&self, l: &Xf) -> Xf {
        Xf { pos: self.pos + self.rot * (self.scale * l.pos), rot: self.rot * l.rot, scale: self.scale * l.scale }
    }

    fn local_of(&self, m: &Xf) -> Xf {
        let inv = self.rot.conjugate();
        Xf { pos: (inv * (m.pos - self.pos)) / self.scale, rot: inv * m.rot, scale: m.scale / self.scale }
    }

    pub fn mat(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rot, self.pos)
    }
}

/// Local and model-space joint transforms for one skeleton.
pub struct PoseBuf {
    pub parents: Vec<i16>,
    pub local: Vec<Xf>,
    pub model: Vec<Xf>,
}

fn q(v: [f32; 4]) -> Quat {
    Quat::from_xyzw(v[0], v[1], v[2], v[3])
}

impl PoseBuf {
    /// The skeleton's bind pose overridden by an animation sample.
    pub fn new(skel: &Md6Skel, pose: Option<&Pose>) -> Self {
        let n = skel.names.len();
        let mut local: Vec<Xf> = (0..n)
            .map(|j| Xf { pos: Vec3::from(skel.translations[j]), rot: q(skel.rotations[j]).normalize(), scale: Vec3::from(skel.scales[j]) })
            .collect();
        if let Some(p) = pose {
            for &(j, r) in &p.rot {
                if let Some(x) = local.get_mut(j as usize) {
                    x.rot = q(r).normalize();
                }
            }
            for &(j, t) in &p.trans {
                if let Some(x) = local.get_mut(j as usize) {
                    x.pos = Vec3::from(t);
                }
            }
            for &(j, s) in &p.scale {
                if let Some(x) = local.get_mut(j as usize) {
                    x.scale = Vec3::from(s);
                }
            }
        }
        let mut b = PoseBuf { parents: skel.parents.clone(), model: local.clone(), local };
        b.fk();
        b
    }

    fn parent(&self, j: usize) -> Option<usize> {
        let p = self.parents[j];
        (p >= 0 && (p as usize) < j).then_some(p as usize)
    }

    pub fn fk(&mut self) {
        for j in 0..self.local.len() {
            self.model[j] = match self.parent(j) {
                Some(p) => self.model[p].child(&self.local[j]),
                None => self.local[j],
            };
        }
    }

    /// Writes a model transform; descendants follow through their unchanged locals.
    pub fn set_model(&mut self, j: usize, m: Xf) {
        self.model[j] = m;
        self.local[j] = match self.parent(j) {
            Some(p) => self.model[p].local_of(&m),
            None => m,
        };
        let n = self.local.len();
        let mut moved = vec![false; n];
        moved[j] = true;
        for k in j + 1..n {
            if let Some(p) = self.parent(k) {
                if moved[p] {
                    self.model[k] = self.model[p].child(&self.local[k]);
                    moved[k] = true;
                }
            }
        }
    }

    /// 0x141a46a80: sets a joint's local rotation while its direct children keep their model transforms.
    fn set_local_rot_keep_children(&mut self, c: usize, r: Quat) {
        self.local[c].rot = r;
        self.model[c] = match self.parent(c) {
            Some(p) => self.model[p].child(&self.local[c]),
            None => self.local[c],
        };
        for k in c + 1..self.local.len() {
            if self.parent(k) == Some(c) {
                self.local[k] = self.model[c].local_of(&self.model[k]);
            }
        }
    }

    pub fn mats(&self) -> Vec<Mat4> {
        self.model.iter().map(Xf::mat).collect()
    }
}

fn inv_len(v: Vec3) -> f32 {
    let l2 = v.length_squared();
    if l2 <= TINY { 0.0 } else { 1.0 / l2.sqrt() }
}

/// Engine perpendicular used by the shortest-arc builders when the vectors are opposite.
fn perp(u: Vec3) -> Vec3 {
    let (ax, ay, az) = (u.x.abs(), u.y.abs(), u.z.abs());
    if ay < ax || az < ax {
        if ax < ay || az < ay { Vec3::new(-u.y, u.x, 0.0) } else { Vec3::new(-u.z, 0.0, u.x) }
    } else {
        Vec3::new(0.0, -u.z, u.y)
    }
}

/// Shortest-arc rotation taking unit `u` onto unit `v`, as the engine builds it.
fn arc(u: Vec3, v: Vec3) -> Quat {
    let k = u.dot(v) + 1.0;
    let (axis, w) = if k <= EPS { (perp(u), 0.0) } else { (u.cross(v), k) };
    let l2 = axis.length_squared() + w * w;
    let s = if l2 <= TINY { 0.0 } else { 1.0 / l2.sqrt() };
    Quat::from_xyzw(axis.x * s, axis.y * s, axis.z * s, w * s)
}

fn rsqrt_clamped(x: f32) -> f32 {
    1.0 / x.max(TINY).sqrt()
}

/// `idRigIk2Segments` descriptor (0x141a42bc0), built from the setup pose.
pub struct Ik2 {
    parent: usize,
    target: usize,
    pole: usize,
    root: usize,
    mid: usize,
    end: usize,
    end_ofs: Vec3,
    axis: Vec3,
    u1: Vec3,
    l1: f32,
    l2: f32,
    cos0: f32,
    root_local: Quat,
    mid_local: Quat,
    ofs1: Vec3,
    ofs2: Vec3,
}

impl Ik2 {
    pub fn new(m: &[Xf], parent: usize, root: usize, mid: usize, end: usize, target: usize, pole: usize) -> Self {
        let (qp, qr, qm) = (m[parent].rot, m[root].rot, m[mid].rot);
        let (pr, pm, pe) = (m[root].pos, m[mid].pos, m[end].pos);
        let ir = qr.conjugate();
        let a = pr - pm;
        let b = pe - pm;
        let axis = ir * ((a * inv_len(a)).cross(b * inv_len(b)));
        let axis = axis * inv_len(axis);
        let u1 = ir * (pm - pr);
        let l1 = u1.length();
        let u1 = u1 / l1;
        let u2 = ir * (pm - pe);
        let l2 = u2.length();
        let u2 = u2 / l2;
        Ik2 {
            parent,
            target,
            pole,
            root,
            mid,
            end,
            end_ofs: ir * (pe - pr),
            axis,
            u1,
            l1,
            l2,
            cos0: u1.dot(u2).clamp(-1.0, 1.0),
            root_local: qp.conjugate() * qr,
            mid_local: ir * qm,
            ofs1: ir * (pm - pr),
            ofs2: qm.conjugate() * (pe - pm),
        }
    }

    /// 0x141a41ec0: elbow bend and whole-chain swing for a root rotation `q`.
    fn bend_swing(&self, target: Vec3, q: Quat, root: Vec3) -> (Quat, Quat) {
        let d = root - target;
        if d.abs().max_element() <= EPS {
            return (Quat::IDENTITY, Quat::IDENTITY);
        }
        let len2 = d.length_squared();
        let inv = rsqrt_clamped(len2);
        let dhat = d * inv;
        let dist = inv * len2;
        let c = ((self.l1 * self.l1 + self.l2 * self.l2 - dist * dist) / (self.l1 * self.l2 + self.l1 * self.l2)).clamp(-1.0, 1.0);
        let s2 = (1.0 - self.cos0 * self.cos0) * (1.0 - c * c);
        let h = ((rsqrt_clamped(s2) * s2 + self.cos0 * c + 1.0) * 0.5).abs();
        let w = rsqrt_clamped(h) * h;
        let t = (1.0 - w * w).abs();
        let sign = if self.cos0 - c > 0.0 { 1.0 } else if self.cos0 - c < 0.0 { -1.0 } else { 0.0 };
        let sin = rsqrt_clamped(t) * t * sign;
        let ax = (q * self.axis) * sin;
        let bend = Quat::from_xyzw(ax.x, ax.y, ax.z, w);
        let mid = root + (q * self.u1) * self.l1;
        let end = root + q * self.end_ofs;
        let lower = bend * (end - mid);
        let e = root - (mid + lower);
        let e = e * rsqrt_clamped(e.length_squared());
        (bend, arc(e, dhat))
    }

    /// 0x141a451f0 + 0x141a428b0 for one instance.
    pub fn solve(&self, pb: &mut PoseBuf, skip_pole: bool) {
        let root_pos = pb.model[self.root].pos;
        let tgt = pb.model[self.target];
        let mut pole = pb.local[self.pole].rot;
        if pole.x < 0.0 {
            pole = Quat::from_xyzw(-pole.x, pole.y, pole.z, -pole.w);
        }
        let to = tgt.pos - root_pos;
        let dir = to * inv_len(to) * pole.x;
        let mut q = pb.model[self.parent].rot * self.root_local;
        if !skip_pole {
            q = Quat::from_xyzw(dir.x, dir.y, dir.z, pole.w) * q;
        }
        let (bend, swing) = self.bend_swing(tgt.pos, q, root_pos);
        let mid_world = q * self.mid_local;
        let mid_local = q.conjugate() * (bend * mid_world);
        let root_rot = swing * q;
        let mid_rot = root_rot * mid_local;
        let mid_pos = root_pos + root_rot * self.ofs1;
        let end_pos = mid_pos + mid_rot * self.ofs2;
        let r = pb.model[self.root];
        pb.set_model(self.root, Xf { rot: root_rot, ..r });
        let m = pb.model[self.mid];
        pb.set_model(self.mid, Xf { pos: mid_pos, rot: mid_rot, ..m });
        let e = pb.model[self.end];
        pb.set_model(self.end, Xf { pos: end_pos, rot: tgt.rot, ..e });
    }
}

/// Copy constraint (0x141a3efe0 / 0x141a3f320, evaluated by 0x141a44910): `dst` = `src` · offset.
struct Copy {
    src: usize,
    dst: usize,
    ofs: Vec3,
    rel: Quat,
}

impl Copy {
    fn relative(m: &[Xf], src: usize, dst: usize) -> Self {
        let ia = m[src].rot.conjugate();
        Copy { src, dst, ofs: ia * (m[dst].pos - m[src].pos), rel: ia * m[dst].rot }
    }

    fn exact(src: usize, dst: usize) -> Self {
        Copy { src, dst, ofs: Vec3::ZERO, rel: Quat::IDENTITY }
    }

    fn apply(&self, pb: &mut PoseBuf) {
        let a = pb.model[self.src];
        let d = pb.model[self.dst];
        // Note the engine composes the stored relative rotation on the left.
        pb.set_model(self.dst, Xf { pos: a.pos + a.rot * self.ofs, rot: self.rel * a.rot, ..d });
    }
}

/// Twist distribution node (0x141a3f920, evaluated by 0x141a45920) with its flag clear.
struct Twist {
    hand: usize,
    forearm: usize,
    roll: usize,
    rest: Quat,
    weight: f32,
}

impl Twist {
    const AXIS: Vec3 = Vec3::new(0.0, -1.0, 0.0);

    fn new(m: &[Xf], hand: usize, forearm: usize, roll: usize, weight: f32) -> Self {
        let r = m[forearm].rot.conjugate() * m[roll].rot;
        let l2 = r.length_squared();
        let s = if l2 <= TINY { 0.0 } else { 1.0 / l2.sqrt() };
        Twist { hand, forearm, roll, rest: r * s, weight }
    }

    fn apply(&self, pb: &mut PoseBuf) {
        let qa = pb.model[self.hand].rot;
        let qb = pb.model[self.forearm].rot;
        let r0 = qb * self.rest;
        let h = qa * Self::AXIS;
        let f = qb * Self::AXIS;
        let c = r0 * Self::AXIS;
        let t1 = arc(c, f) * r0;
        let mut t2 = arc(h, f) * qa;
        if t1.dot(t2) >= 0.0 {
            t2 = -t2;
        }
        let r = t1 * (1.0 - self.weight) + t2 * -self.weight;
        let r = r * rsqrt_clamped(r.length_squared());
        let local = match pb.parent(self.roll) {
            Some(p) => pb.model[p].rot.conjugate() * r,
            None => r,
        };
        pb.set_local_rot_keep_children(self.roll, local);
    }
}

struct Arm {
    ik: Ik2,
    copies: Vec<Copy>,
    twists: Vec<Twist>,
}

/// The per-skeleton rig program for both arms.
pub struct ArmRig {
    arms: Vec<Arm>,
}

impl ArmRig {
    pub fn new(skel: &Md6Skel) -> Option<Self> {
        let find = |n: &str| skel.names.iter().position(|x| x.eq_ignore_ascii_case(n));
        let mut bind = PoseBuf::new(skel, None);
        let mut arms = Vec::new();
        for side in ["Left", "Right"] {
            let lo = side.to_ascii_lowercase();
            let (Some(hand), Some(forearm), Some(arm)) = (find(&format!("{side}Hand")), find(&format!("{side}ForeArm")), find(&format!("{side}Arm"))) else { continue };
            let rig = |s: &str| find(&format!("rig_arm_{lo}_{s}"));
            let (Some(target), Some(pole), Some(root), Some(hinge)) = (rig("target"), rig("pole"), rig("root"), rig("hinge")) else { continue };
            if find(&format!("{lo}handattach")).is_none() {
                continue;
            }
            // Setup pose: rig root := upper arm, rig hinge := forearm (model space).
            let (a, f) = (bind.model[arm], bind.model[forearm]);
            bind.set_model(root, a);
            bind.set_model(hinge, f);
            let parent = skel.parents[root].max(0) as usize;
            let ik = Ik2::new(&bind.model, parent, root, hinge, hand, target, pole);
            let rolls: Vec<usize> = ["ForeArmRoll3", "ForeArmRoll2", "ForeArmRoll1", "ForeArmRoll"].iter().filter_map(|r| find(&format!("{side}{r}"))).collect();
            let mut copies: Vec<Copy> = rolls.iter().map(|&r| Copy::relative(&bind.model, hinge, r)).collect();
            copies.push(Copy::exact(hinge, forearm));
            copies.push(Copy::exact(root, arm));
            let weights = [("ForeArmRoll3", 1.0), ("ForeArmRoll2", 0.75), ("ForeArmRoll1", 0.5), ("ForeArmRoll", 0.25)];
            let twists = weights.iter().filter_map(|(r, w)| Some(Twist::new(&bind.model, hand, forearm, find(&format!("{side}{r}"))?, *w))).collect();
            arms.push(Arm { ik, copies, twists });
        }
        (!arms.is_empty()).then_some(ArmRig { arms })
    }

    pub fn apply(&self, pb: &mut PoseBuf) {
        for arm in &self.arms {
            arm.ik.solve(pb, false);
            for c in &arm.copies {
                c.apply(pb);
            }
            for t in &arm.twists {
                t.apply(pb);
            }
        }
    }
}
