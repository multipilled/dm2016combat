//! Environment decls (`generated/decls/env/<name>.decl`): `renderParms { key value }` text with
//! `inherit { <env> }`. The level's worldspawn names one in `edit.envSettings`; without one the engine's
//! `default` env applies. FX use the transparency multipliers it sets for the transsort renderparms.

use idres::Container;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvParms {
    /// $particleMult: non-emissive particles' colour scale (transsortblend).
    pub particle_mult: f32,
    /// $decalMult.
    pub decal_mult: f32,
    /// $transparencyLightMult: fogged transparencies.
    pub transparency_light_mult: f32,
}

/// The default env's values (generated/decls/env/default.decl; also the exe's built-in env block).
pub const DEFAULT: EnvParms = EnvParms { particle_mult: 0.15, decal_mult: 0.15, transparency_light_mult: 0.15 };

fn lookup(c: &Container, env: &str, key: &str) -> Option<f32> {
    let mut name = Some(env.to_string());
    for _ in 0..8 {
        let n = name.take()?;
        let text = c.read_by_name(&format!("generated/decls/env/{n}.decl")).ok()?;
        let text = String::from_utf8_lossy(&text);
        let toks: Vec<&str> = text.split(|ch: char| ch.is_whitespace() || ch == '{' || ch == '}').filter(|t| !t.is_empty()).collect();
        if let Some(i) = toks.iter().position(|t| t.eq_ignore_ascii_case(key)) {
            return toks.get(i + 1).and_then(|v| v.parse().ok());
        }
        name = toks.iter().position(|t| *t == "inherit").and_then(|i| toks.get(i + 1)).map(|t| t.trim_matches('"').to_string());
    }
    None
}

/// Values of env `name` along its inherit chain, falling back to the `default` env, then DEFAULT.
pub fn load(c: &Container, name: Option<&str>) -> EnvParms {
    let get = |key: &str, d: f32| name.and_then(|n| lookup(c, n, key)).or_else(|| lookup(c, "default", key)).unwrap_or(d);
    EnvParms {
        particle_mult: get("particleMult", DEFAULT.particle_mult),
        decal_mult: get("decalMult", DEFAULT.decal_mult),
        transparency_light_mult: get("transparencyLightMult", DEFAULT.transparency_light_mult),
    }
}

/// The worldspawn's `envSettings` of `maps/<map>.entities` (the first entity), without parsing the file.
pub fn map_env(c: &Container, map: &str) -> Option<String> {
    let text = c.read_by_name(&format!("maps/{map}.entities")).ok()?;
    let head = &text[..text.len().min(1 << 16)];
    let s = String::from_utf8_lossy(head);
    let i = s.find("envSettings")?;
    let rest = &s[i..];
    let a = rest.find('"')? + 1;
    let b = rest[a..].find('"')? + a;
    Some(rest[a..b].to_string())
}
