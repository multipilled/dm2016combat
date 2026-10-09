//! md6Def decls (`generated/decls/md6def/<name>.md6.decl`): mesh, offset, inheritance and the
//! per-animation frame events (`ae_*`) the game dispatches while an anim plays.

use std::collections::HashMap;

use anyhow::Result;

use crate::ldecl::{self, Arg, Item};

#[derive(Debug, Clone, PartialEq)]
pub struct AnimEvent {
    /// `ae_fireWeaponRight`, `ae_soundWeapon`, ...
    pub name: String,
    pub frame: f32,
    pub row: i32,
    /// Remaining keys in decl order, e.g. `("sound", "play_wpn_har_dryfire")`, `("int", "1")`.
    pub params: Vec<(String, Arg)>,
}

impl AnimEvent {
    pub fn param(&self, key: &str) -> Option<&Arg> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Md6DefDecl {
    pub inherit: Option<String>,
    pub mesh: Option<String>,
    pub offset: Option<[f32; 3]>,
    /// md6anim name → events, in decl order.
    pub events: HashMap<String, Vec<AnimEvent>>,
    /// alias name → md6anim name.
    pub aliases: HashMap<String, String>,
}

impl Md6DefDecl {
    pub fn parse(src: &str) -> Result<Self> {
        let root = ldecl::parse(src)?;
        let init = root.child("init");
        let mut d = Md6DefDecl {
            inherit: init.and_then(|i| i.child("inherit")).and_then(Item::text).map(str::to_string),
            mesh: init.and_then(|i| i.child("mesh")).and_then(Item::text).map(str::to_string),
            offset: init.and_then(|i| i.child("offset")).and_then(|o| match o.arg(0) {
                Some(Arg::Tuple(t)) if t.len() == 3 => Some([t[0], t[1], t[2]]),
                _ => None,
            }),
            ..Default::default()
        };
        if let Some(ev) = root.child("events") {
            for a in ev.children_named("anim") {
                let Some(anim) = a.text() else { continue };
                let list = a
                    .children_named("event")
                    .map(|e| AnimEvent {
                        name: e.text().unwrap_or("").to_string(),
                        frame: e.child("frame").and_then(Item::f32).unwrap_or(0.0),
                        row: e.child("row").and_then(Item::f32).unwrap_or(0.0) as i32,
                        params: e
                            .children
                            .iter()
                            .filter(|c| !matches!(c.key.as_str(), "frame" | "row" | "locked"))
                            .map(|c| (c.key.clone(), c.args.first().cloned().unwrap_or(Arg::Atom(String::new()))))
                            .collect(),
                    })
                    .collect();
                d.events.insert(anim.to_string(), list);
            }
        }
        if let Some(al) = root.child("aliases") {
            for a in al.children_named("alias") {
                if let (Some(n), Some(an)) = (a.child("name").and_then(Item::text), a.child("anim").and_then(Item::text)) {
                    d.aliases.insert(n.to_string(), an.to_string());
                }
            }
        }
        Ok(d)
    }

    /// Overlays `child` (which inherits from `self`): child fields win, event lists are replaced per anim.
    pub fn merged_with(&self, child: &Md6DefDecl) -> Md6DefDecl {
        let mut out = self.clone();
        out.inherit = child.inherit.clone();
        if child.mesh.is_some() {
            out.mesh = child.mesh.clone();
        }
        if child.offset.is_some() {
            out.offset = child.offset;
        }
        for (k, v) in &child.events {
            out.events.insert(k.clone(), v.clone());
        }
        for (k, v) in &child.aliases {
            out.aliases.insert(k.clone(), v.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events() {
        let src = "{\n\tinit {\n\t\tinherit \"player/fp_hands.md6\"\n\t\tmesh \"m.md6mesh\"\n\t\toffset ( -12.6 0 -83.5 )\n\t}\n\tevents {\n\t\tanim \"a.md6anim\" {\n\t\t\tevent \"ae_soundWeapon\" {\n\t\t\t\tframe 4\n\t\t\t\trow 1\n\t\t\t\tlocked 0\n\t\t\t\tsound \"play_x\"\n\t\t\t}\n\t\t}\n\t}\n\teyeInfoCollection 0 {\n\t}\n}\n";
        let d = Md6DefDecl::parse(src).unwrap();
        assert_eq!(d.inherit.as_deref(), Some("player/fp_hands.md6"));
        assert_eq!(d.offset, Some([-12.6, 0.0, -83.5]));
        let e = &d.events["a.md6anim"][0];
        assert_eq!((e.name.as_str(), e.frame, e.row), ("ae_soundWeapon", 4.0, 1));
        assert_eq!(e.param("sound").and_then(Arg::text), Some("play_x"));
    }
}
