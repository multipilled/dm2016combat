//! Particle material decls (`generated/decls/material/<name>.decl`, `key value` lines).
//!
//! Particle materials use the transparency-sort stage programs (renderprog `transsortblend`): an RG
//! texture (R = brightness, G = opacity) from the transsort atlas, blended ONE / ONE_MINUS_SRC_ALPHA
//! with premultiplied output, and `emissive` / `emissivemult` selecting between the emissive multiplier
//! and the scene's particle lighting multiplier. Each atlas entry also exists as its own image:
//! `transatlasmap "textures/x/Y.tga"` -> `generated/transsortatlas/textures/x/y.bimage`.

use anyhow::{Context, Result};
use idres::Container;

#[derive(Debug, Clone, PartialEq)]
pub struct ParticleMaterial {
    pub name: String,
    pub stage_program: String,
    /// Resource of the material's own transsort image.
    pub image: Option<String>,
    pub emissive: f32,
    /// None: the $emissiveMult renderparm default (5.0, renderparm/emissivemult.decl).
    pub emissive_mult: Option<f32>,
    pub uber1: [f32; 4],
    pub uber2: f32,
    pub uber3: [f32; 4],
    /// `factor` ($factor, renderparm default 1).
    pub factor: Option<f32>,
}

impl ParticleMaterial {
    /// GPU particle stage programs (gpuparticle/add, gpuparticle/blend): `texturemap` sampled RG
    /// (R = brightness, G = opacity) without DeGamma.
    pub fn is_gpu(&self) -> bool {
        self.stage_program.to_ascii_lowercase().starts_with("gpuparticle/")
    }
    pub fn is_gpu_add(&self) -> bool {
        self.stage_program.eq_ignore_ascii_case("gpuparticle/add")
    }
}

fn floats(v: &str) -> Vec<f32> {
    v.trim_matches(|c| c == '{' || c == '}' || c == ' ').split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

impl ParticleMaterial {
    pub fn load(c: &Container, name: &str) -> Result<ParticleMaterial> {
        let path = format!("generated/decls/material/{name}.decl");
        let text = String::from_utf8_lossy(&c.read_by_name(&path).with_context(|| path.clone())?).into_owned();
        let mut m = ParticleMaterial { name: name.to_string(), stage_program: String::new(), image: None, emissive: 0.0, emissive_mult: None, uber1: [0.0; 4], uber2: 0.0, uber3: [0.0; 4], factor: None };
        for line in text.lines() {
            let line = line.trim();
            let Some((k, v)) = line.split_once(char::is_whitespace) else { continue };
            let v = v.trim();
            match k.to_ascii_lowercase().as_str() {
                "stageprogram" => m.stage_program = v.to_string(),
                "transatlasmap" => {
                    let src = v.trim_matches('"').to_ascii_lowercase();
                    let stem = src.strip_suffix(".tga").unwrap_or(&src);
                    m.image = Some(format!("generated/transsortatlas/{stem}.bimage"));
                }
                // GPU particle materials: `texturemap "path.tga$luminancealpha"` -> generated/image/path.tga$luminancealpha
                // .bimage (images with options keep the extension); `texturemap "path.tga"` -> path.bimage.
                "texturemap" if m.image.is_none() => {
                    let src = v.trim_matches('"').to_ascii_lowercase();
                    let stem = if src.contains('$') { src.as_str() } else { src.strip_suffix(".tga").unwrap_or(&src) };
                    m.image = Some(format!("generated/image/{stem}.bimage"));
                }
                "factor" => m.factor = v.parse().ok(),
                "emissive" => m.emissive = v.parse().unwrap_or(0.0),
                "emissivemult" => m.emissive_mult = v.parse().ok(),
                "particleuberparms1" => {
                    let f = floats(v);
                    for (i, x) in f.iter().take(4).enumerate() {
                        m.uber1[i] = *x;
                    }
                }
                "particleuberparms2" => m.uber2 = floats(v).first().copied().unwrap_or(0.0),
                "particleuberparms3" => {
                    let f = floats(v);
                    for (i, x) in f.iter().take(4).enumerate() {
                        m.uber3[i] = *x;
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }
}
