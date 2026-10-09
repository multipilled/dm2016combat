//! Hit volumes: what a hitscan or projectile hits on an actor. Damage_Calculate takes the hit joint from
//! trace_t.c.trmFeature when c.type == CONTACT_SPHERE (4); the collision code that produces those contacts
//! (0x141956d69, a chunk of 0x141956670) intersects the ray with spheres placed by per-joint 3x4 matrices and the
//! entity axis/origin, writing the sphere's joint byte into trmFeature. The spheres are the md6Def hitTestGroup
//! entries (joint, joint-space offset, radius).
//! INFERRED: every hitTestGroup takes part (incl. the gore_* wound spheres); first entry point along the ray, a
//! start inside a sphere hits at distance 0 (the exe's sphere/ray arithmetic is not ported).

use glam::{Mat4, Vec3};

use super::decl::{GroupKind, JointGroups};

/// One sphere of a hitTestGroup, resolved to a skeleton joint.
#[derive(Debug, Clone, PartialEq)]
pub struct HitSphere {
    pub group: String,
    pub joint: usize,
    pub joint_name: String,
    pub offset: Vec3,
    pub radius: f32,
}

/// Builds the hit spheres for a skeleton (joint names compared case-insensitively; unknown joints skipped).
pub fn spheres(groups: &JointGroups, joint_names: &[String]) -> Vec<HitSphere> {
    let mut out = Vec::new();
    for g in groups.of_kind(GroupKind::HitTest) {
        for (j, off, r) in &g.spheres {
            if let Some(ji) = joint_names.iter().position(|n| n.eq_ignore_ascii_case(j)) {
                out.push(HitSphere { group: g.name.clone(), joint: ji, joint_name: joint_names[ji].clone(), offset: Vec3::from(*off), radius: *r });
            }
        }
    }
    out
}

/// The spheres of one traceGroup (md6Def JOINTGROUP_TRACE, the SphereModelTrace joint groups), resolved like
/// [`spheres`].
pub fn trace_spheres(groups: &JointGroups, name: &str, joint_names: &[String]) -> Vec<HitSphere> {
    let mut out = Vec::new();
    for g in groups.of_kind(GroupKind::Trace).filter(|g| g.name.eq_ignore_ascii_case(name)) {
        for (j, off, r) in &g.spheres {
            if let Some(ji) = joint_names.iter().position(|n| n.eq_ignore_ascii_case(j)) {
                out.push(HitSphere { group: g.name.clone(), joint: ji, joint_name: joint_names[ji].clone(), offset: Vec3::from(*off), radius: *r });
            }
        }
    }
    out
}

/// A sphere the ray hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SphereHit {
    pub dist: f32,
    pub point: Vec3,
    /// Unit normal at the hit (point - centre).
    pub normal: Vec3,
    pub sphere: usize,
    pub joint: usize,
}

/// Ray (unit `dir`) against one sphere: the entry distance in [0, max].
pub fn ray_sphere(start: Vec3, dir: Vec3, max: f32, centre: Vec3, r: f32) -> Option<f32> {
    let m = start - centre;
    let c = m.length_squared() - r * r;
    if c <= 0.0 {
        return Some(0.0);
    }
    let b = m.dot(dir);
    if b > 0.0 {
        return None;
    }
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    (t <= max).then_some(t.max(0.0))
}

/// Nearest sphere hit. `model_to_world` places the model (origin, yaw, md6Def offset); `joints` are the
/// joints' model-space matrices of the current pose.
pub fn trace(spheres: &[HitSphere], joints: &[Mat4], model_to_world: Mat4, start: Vec3, dir: Vec3, max: f32) -> Option<SphereHit> {
    let mut best: Option<SphereHit> = None;
    for (i, s) in spheres.iter().enumerate() {
        let Some(jm) = joints.get(s.joint) else { continue };
        let centre = (model_to_world * *jm).transform_point3(s.offset);
        if let Some(t) = ray_sphere(start, dir, max, centre, s.radius)
            && best.as_ref().is_none_or(|b| t < b.dist)
        {
            let point = start + dir * t;
            best = Some(SphereHit { dist: t, point, normal: (point - centre).normalize_or(-dir), sphere: i, joint: s.joint });
        }
    }
    best
}

/// World-space centres and radii of all spheres (debug drawing, radius damage bounds).
pub fn world_spheres(spheres: &[HitSphere], joints: &[Mat4], model_to_world: Mat4) -> Vec<(Vec3, f32)> {
    spheres.iter().filter_map(|s| joints.get(s.joint).map(|jm| ((model_to_world * *jm).transform_point3(s.offset), s.radius))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_sphere_entry() {
        let t = ray_sphere(Vec3::ZERO, Vec3::X, 100.0, Vec3::new(10.0, 0.0, 0.0), 2.0).unwrap();
        assert!((t - 8.0).abs() < 1e-5);
        assert!(ray_sphere(Vec3::ZERO, Vec3::X, 5.0, Vec3::new(10.0, 0.0, 0.0), 2.0).is_none());
        assert!(ray_sphere(Vec3::ZERO, Vec3::X, 100.0, Vec3::new(10.0, 3.0, 0.0), 2.0).is_none());
        assert_eq!(ray_sphere(Vec3::ZERO, Vec3::X, 100.0, Vec3::new(1.0, 0.0, 0.0), 2.0), Some(0.0));
    }
}
