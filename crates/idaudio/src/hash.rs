//! Wwise IDs and the engine's sound-name → event-name rule.

/// `AK::SoundEngine::GetIDFromString` (exe 0x141d42a50): ASCII `A-Z` lower-cased, then 32-bit FNV-1
/// (multiply by 0x01000193, then xor). Event, switch, state, bus and game-parameter IDs all use this.
pub fn fnv1_lower(name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in name.as_bytes() {
        h = h.wrapping_mul(0x0100_0193) ^ u32::from(b.to_ascii_lowercase());
    }
    h
}

/// Wwise event ID for an event name, e.g. `event_id("play_wpn_sp_shotgun_fire") == 950761463`.
pub fn event_id(name: &str) -> u32 {
    fnv1_lower(name)
}

/// The event name the engine posts for a sound reference found in decls (exe 0x141688330, called from
/// the idDeclSound parse path 0x1416c7f40).
///
/// Names already starting with `play_`/`stop_` (any case) and containing no `/` are used as-is. Anything
/// else is lower-cased with `\` → `/`, reduced to its file name without extension and prefixed with
/// `play_`: `"player/pain/small"` → `"play_small"`, `"footsteps/player/walk/sp_default"` →
/// `"play_sp_default"`.
pub fn sound_event_name(sound: &str) -> String {
    let starts = |p: &str| sound.len() >= p.len() && sound[..p.len()].eq_ignore_ascii_case(p);
    if (starts("play_") || starts("stop_")) && !sound.contains('/') {
        return sound.to_owned();
    }
    let mut s: String = sound.chars().map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() }).collect();
    if !s.starts_with("//") {
        s = s.trim_start_matches('/').to_owned();
    }
    let file = s.rsplit('/').next().unwrap_or("");
    let stem = file.rfind('.').map_or(file, |dot| &file[..dot]);
    format!("play_{stem}")
}

/// `event_id(&sound_event_name(sound))`: the event the engine posts for a decl sound reference.
pub fn sound_event_id(sound: &str) -> u32 {
    fnv1_lower(&sound_event_name(sound))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_soundbanksinfo() {
        // Ids from the install's soundbanksinfo.xml.
        assert_eq!(event_id("Play_wpn_sp_shotgun_fire"), 950_761_463);
        assert_eq!(event_id("play_wpn_pistol_sp_fire"), 1_179_266_350);
        assert_eq!(event_id("Play_gl_smoke_recharged_alert"), 3_164_289_750);
        assert_eq!(fnv1_lower("locality"), 3_823_160_874);
        assert_eq!(fnv1_lower("first_person"), 3_465_771_971);
    }

    #[test]
    fn decl_names_normalise_like_the_engine() {
        assert_eq!(sound_event_name("play_wpn_sp_shotgun_fire"), "play_wpn_sp_shotgun_fire");
        assert_eq!(sound_event_name("Play_wpn_sp_HAR_fire"), "Play_wpn_sp_HAR_fire");
        assert_eq!(sound_event_name("stop_plr_env_damage_fire"), "stop_plr_env_damage_fire");
        assert_eq!(sound_event_name("player/pain/small"), "play_small");
        assert_eq!(sound_event_name("footsteps/player/walk/sp_default"), "play_sp_default");
        assert_eq!(sound_event_name("Effects\\Explosions\\Rocket_Explosion_MP.wav"), "play_rocket_explosion_mp");
        assert_eq!(sound_event_name("play_x/y"), "play_y");
        assert_eq!(sound_event_id("effects/explosions/rocket_explosion_mp"), event_id("Play_rocket_explosion_mp"));
    }
}
