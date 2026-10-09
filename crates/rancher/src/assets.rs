//! Loads DOOM (2016) models from the user's install into Bevy meshes.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use idres::Container;
use idres::md6::{Md6Model, Md6Skel, unpack_normal};

/// The parts of an md6Def decl the view model needs.
#[derive(Debug, Clone, Default)]
pub struct Md6Def {
    pub mesh: String,
    pub offset: [f32; 3],
}

/// md6Def decls use `key "value"` and `key ( x y z )` lines rather than `key = value;`; an
/// `inherit "parent.md6"` line takes the parent's fields first.
pub fn read_md6def(container: &Container, name: &str) -> Result<Md6Def> {
    read_md6def_depth(container, name, 0)
}

fn read_md6def_depth(container: &Container, name: &str, depth: u32) -> Result<Md6Def> {
    anyhow::ensure!(depth < 16, "md6Def inheritance too deep at {name}");
    let path = format!("generated/decls/md6def/{name}.decl");
    let text = String::from_utf8_lossy(&container.read_by_name(&path)?).into_owned();
    let mut def = Md6Def::default();
    let (mut mesh, mut offset) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("inherit ") {
            def = read_md6def_depth(container, rest.trim().trim_matches('"'), depth + 1)?;
        } else if let Some(rest) = line.strip_prefix("mesh ") {
            mesh = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("offset ") {
            let nums: Vec<f32> = rest.trim_matches(|c| c == '(' || c == ')' || c == ' ').split_whitespace().filter_map(|n| n.parse().ok()).collect();
            if nums.len() == 3 {
                offset = Some([nums[0], nums[1], nums[2]]);
            }
        }
        if line.starts_with("events") {
            break;
        }
    }
    if let Some(m) = mesh {
        def.mesh = m;
    }
    if let Some(o) = offset {
        def.offset = o;
    }
    anyhow::ensure!(!def.mesh.is_empty(), "{path} has no mesh");
    Ok(def)
}

/// `md6/.../x.md6mesh` → `generated/basemodel/md6/.../x.bmd6model`.
pub fn model_resource(mesh: &str) -> String {
    format!("generated/basemodel/{}.bmd6model", mesh.trim_end_matches(".md6mesh"))
}

pub fn skeleton_resource(skl: &str) -> String {
    format!("generated/skeleton/{}.bmd6skl", skl.trim_end_matches(".md6skl"))
}

pub struct LoadedModel {
    pub model: Md6Model,
    pub skel: Option<Md6Skel>,
}

pub fn load_model(container: &Container, mesh: &str) -> Result<LoadedModel> {
    let bytes = container.read_by_name(&model_resource(mesh)).with_context(|| format!("model {mesh}"))?;
    let model = Md6Model::parse(&bytes).with_context(|| format!("parsing {mesh}"))?;
    let skel = container.read_by_name(&skeleton_resource(&model.skeleton)).ok().and_then(|b| Md6Skel::parse(&b).ok());
    Ok(LoadedModel { model, skel })
}

/// idTech (x forward, y left, z up) to Bevy (x right, y up, z back).
pub fn to_bevy(v: [f32; 3]) -> [f32; 3] {
    [-v[1], v[2], -v[0]]
}

/// One Bevy mesh per md6 mesh, positioned with `offset` (idTech units, model space).
pub fn bevy_meshes(model: &Md6Model, offset: [f32; 3], hidden: &[&str]) -> Vec<(String, String, Mesh)> {
    let mut out = Vec::new();
    for m in &model.meshes {
        if hidden.contains(&m.name.as_str()) {
            continue;
        }
        let pos: Vec<[f32; 3]> = m.verts.iter().map(|v| to_bevy([v.xyz[0] + offset[0], v.xyz[1] + offset[1], v.xyz[2] + offset[2]])).collect();
        let nrm: Vec<[f32; 3]> = m.verts.iter().map(|v| to_bevy(unpack_normal(v.normal))).collect();
        let uv: Vec<[f32; 2]> = m.verts.iter().map(|v| v.st).collect();
        // idTech→Bevy flips handedness; reverse winding.
        let mut idx: Vec<u32> = Vec::with_capacity(m.indices.len());
        for t in m.indices.chunks_exact(3) {
            idx.extend_from_slice(&[t[0] as u32, t[2] as u32, t[1] as u32]);
        }
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(idx));
        out.push((m.name.clone(), m.material.clone(), mesh));
    }
    out
}

/// Stand-in surface colours until virtual-texture pages are decoded.
pub fn placeholder_color(material: &str, cache: &mut HashMap<String, Color>) -> Color {
    let n = cache.len();
    *cache.entry(material.to_string()).or_insert_with(|| {
        if material.contains("player_naked") || material.contains("arms") || material.contains("hands") {
            Color::srgb(0.18, 0.24, 0.18)
        } else {
            let h = (n as f32 * 47.0) % 360.0;
            Color::hsl(h, 0.08, 0.32)
        }
    })
}
