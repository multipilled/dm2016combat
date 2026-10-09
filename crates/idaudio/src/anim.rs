//! Sound events in md6def decls (`generated/decls/md6def/...md6.decl`): which sound plays on which
//! frame of which animation, e.g. the shotgun's `dryfire.md6anim` posts `play_wpn_sp_shotgun_dry_fire`
//! at frame 0 via `ae_soundWeapon`.
//!
//! The md6def syntax is `events { anim "<path>" { event "<name>" { frame N row R locked L sound "<s>" } } }`.

/// One anim event that names a sound.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimSound {
    /// The `.md6anim` path as written in the decl.
    pub anim: String,
    /// Event kind, e.g. `ae_sound`, `ae_soundWeapon`, `ae_soundBody`.
    pub event: String,
    pub frame: u32,
    pub row: u32,
    /// Sound reference; map it to a Wwise event with [`crate::sound_event_id`].
    pub sound: String,
}

fn tokens(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '/' {
            chars.next();
            match chars.peek() {
                Some('/') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = ' ';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            break;
                        }
                        prev = c;
                    }
                }
                _ => out.push("/".into()),
            }
        } else if c == '"' {
            chars.next();
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            out.push(s);
        } else if "{}()".contains(c) {
            chars.next();
            out.push(c.to_string());
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || "{}()\"".contains(c) {
                    break;
                }
                s.push(c);
                chars.next();
            }
            out.push(s);
        }
    }
    out
}

/// Every `sound` in an md6def's `events` block.
pub fn md6def_sounds(src: &str) -> Vec<AnimSound> {
    let t = tokens(src);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut events_depth = None;
    let mut anim: Option<(String, usize)> = None;
    let mut event: Option<(String, usize, u32, u32, Vec<String>)> = None;
    let mut i = 0;
    while i < t.len() {
        let tok = t[i].as_str();
        match tok {
            "{" => depth += 1,
            "}" => {
                if let Some((name, d, frame, row, sounds)) = &event
                    && *d == depth
                {
                    if let Some((a, _)) = &anim {
                        for s in sounds {
                            out.push(AnimSound { anim: a.clone(), event: name.clone(), frame: *frame, row: *row, sound: s.clone() });
                        }
                    }
                    event = None;
                }
                if anim.as_ref().is_some_and(|(_, d)| *d == depth) {
                    anim = None;
                }
                if events_depth == Some(depth) {
                    events_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            "events" if t.get(i + 1).is_some_and(|n| n == "{") => events_depth = Some(depth + 1),
            "anim" if events_depth.is_some() && t.get(i + 2).is_some_and(|n| n == "{") => {
                anim = Some((t[i + 1].clone(), depth + 1));
                i += 1;
            }
            "event" if anim.is_some() && t.get(i + 2).is_some_and(|n| n == "{") => {
                event = Some((t[i + 1].clone(), depth + 1, 0, 0, Vec::new()));
                i += 1;
            }
            "frame" | "row" | "sound" if event.is_some() => {
                if let (Some(v), Some(ev)) = (t.get(i + 1), event.as_mut()) {
                    match tok {
                        "frame" => ev.2 = v.parse().unwrap_or(0),
                        "row" => ev.3 = v.parse().unwrap_or(0),
                        _ => ev.4.push(v.clone()),
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anim_sound_events() {
        let src = r#"{
	events {
		anim "md6/a/shoot.md6anim" {
			event "ae_ejectShell" {
				frame 17
				row 0
				locked 0
			}
		}
		anim "md6/a/dryfire.md6anim" {
			event "ae_soundWeapon" {
				frame 3
				row 1
				locked 0
				sound "play_wpn_sp_shotgun_dry_fire"
			}
		}
	}
}"#;
        let s = md6def_sounds(src);
        assert_eq!(
            s,
            vec![AnimSound {
                anim: "md6/a/dryfire.md6anim".into(),
                event: "ae_soundWeapon".into(),
                frame: 3,
                row: 1,
                sound: "play_wpn_sp_shotgun_dry_fire".into()
            }]
        );
    }
}
