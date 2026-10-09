//! Greybox firing range for movement and weapon testing, in idTech units (Z up).
//! Each feature isolates one mechanic: step heights around the 16-unit step size, ledges around the
//! single and double jump heights, ramps either side of the 0.7 walkable normal, and distance markers
//! for damage falloff.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rancher_sim::collision::{Hull, World};
use rancher_sim::Vec3 as V;

/// idTech (x forward, y left, z up) to Bevy (x right, y up, z back).
pub fn to_bevy(v: V) -> Vec3 {
    Vec3::new(-v.y, v.z, -v.x)
}

pub struct Brush {
    pub hull: Hull,
    pub color: Color,
}

fn boxb(min: [f32; 3], max: [f32; 3], color: Color) -> Brush {
    Brush { hull: Hull::cuboid(V::from(min), V::from(max)), color }
}

fn ramp(x0: f32, x1: f32, y0: f32, y1: f32, rise: f32, color: Color) -> Brush {
    let pts = [
        V::new(x0, y0, 0.0),
        V::new(x1, y0, 0.0),
        V::new(x0, y1, 0.0),
        V::new(x1, y1, 0.0),
        V::new(x1, y0, rise),
        V::new(x1, y1, rise),
    ];
    Brush { hull: Hull::from_points(&pts), color }
}

pub fn build() -> Vec<Brush> {
    let floor = Color::srgb(0.32, 0.32, 0.34);
    let wall = Color::srgb(0.22, 0.22, 0.25);
    let step = Color::srgb(0.55, 0.42, 0.25);
    let ledge = Color::srgb(0.25, 0.45, 0.55);
    let rampc = Color::srgb(0.45, 0.55, 0.30);
    let marker = Color::srgb(0.75, 0.20, 0.15);
    let mut b = vec![
        boxb([-1024.0, -1536.0, -64.0], [3072.0, 1536.0, 0.0], floor),
        boxb([-1088.0, -1536.0, 0.0], [-1024.0, 1536.0, 512.0], wall),
        boxb([3072.0, -1536.0, 0.0], [3136.0, 1536.0, 512.0], wall),
        boxb([-1024.0, -1600.0, 0.0], [3072.0, -1536.0, 512.0], wall),
        boxb([-1024.0, 1536.0, 0.0], [3072.0, 1600.0, 512.0], wall),
    ];
    // Steps: 8, 12, 16 (= pm_stepsize), 17, 20, 24 units high.
    for (i, h) in [8.0, 12.0, 16.0, 17.0, 20.0, 24.0].into_iter().enumerate() {
        let y = 256.0 + i as f32 * 96.0;
        b.push(boxb([128.0, y, 0.0], [256.0, y + 64.0, h], step));
    }
    // Staircase of 16-unit steps.
    for i in 0..8 {
        let x = 384.0 + i as f32 * 32.0;
        b.push(boxb([x, 256.0, 0.0], [x + 32.0, 384.0, 16.0 * (i + 1) as f32], step));
    }
    // Ledges for single jump (72), double jump (+72), and ledge-grab heights.
    for (i, h) in [48.0, 64.0, 72.0, 80.0, 96.0, 128.0, 144.0, 160.0, 192.0].into_iter().enumerate() {
        let y = -256.0 - i as f32 * 128.0;
        b.push(boxb([128.0, y - 96.0, 0.0], [320.0, y, h], ledge));
    }
    // Ramps: rise over 256 run → normals around the 0.7 walkable limit.
    for (i, rise) in [128.0, 224.0, 256.0, 288.0].into_iter().enumerate() {
        let y = 256.0 + i as f32 * 160.0;
        b.push(ramp(768.0, 1024.0, y, y + 128.0, rise, rampc));
        b.push(boxb([1024.0, y, 0.0], [1088.0, y + 128.0, rise], rampc));
    }
    // Distance markers every 128 units down the firing lane (y = 0), for falloff checks.
    for i in 1..=16 {
        let x = i as f32 * 128.0;
        let h = if i % 4 == 0 { 24.0 } else { 6.0 };
        b.push(boxb([x - 2.0, -96.0, 0.0], [x + 2.0, -88.0, h], marker));
    }
    b
}

pub fn world(brushes: &[Brush]) -> World {
    let mut w = World::default();
    for b in brushes {
        w.add(b.hull.clone());
    }
    w
}

/// Triangulates a convex hull's faces into a flat-shaded mesh in Bevy space.
pub fn mesh(hull: &Hull) -> Mesh {
    let mut pos = Vec::new();
    let mut nrm = Vec::new();
    let mut idx = Vec::new();
    for (n, on) in &hull.faces {
        let pts: Vec<V> = on.iter().map(|&i| hull.verts[i]).collect();
        let c = pts.iter().copied().sum::<V>() / pts.len() as f32;
        let t = (pts[0] - c).normalize();
        let bt = n.cross(t);
        let mut sorted = pts.clone();
        sorted.sort_by(|a, b| {
            let aa = (*a - c).dot(bt).atan2((*a - c).dot(t));
            let ab = (*b - c).dot(bt).atan2((*b - c).dot(t));
            aa.partial_cmp(&ab).unwrap()
        });
        let base = pos.len() as u32;
        for p in &sorted {
            pos.push(to_bevy(*p).to_array());
            nrm.push(to_bevy(*n).to_array());
        }
        // idTech→Bevy flips handedness, so wind the fan the other way.
        for k in 1..sorted.len() as u32 - 1 {
            idx.extend_from_slice(&[base, base + k + 1, base + k]);
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
        .with_inserted_indices(Indices::U32(idx))
}
