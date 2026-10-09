//! Model tags from md6Def decls: `props { prop "<name>" { tag "<tag>" { trans ( x y z ) rot ( qx qy qz qw )
//! parent "<joint>" } } }`. FX look tags up in the `_info` prop (FUN_1403d1660 passes "_info", global
//! 0x14388b660; "Model '%s' has no tag info for prop '%s', tag '%s'."). `inherit "<parent md6>"` lines pull
//! the parent's props first.

use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use idres::Container;

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub name: String,
    /// Offset in the parent joint's space (idTech axes).
    pub trans: [f32; 3],
    /// Rotation relative to the parent joint, x y z w as written. The axes are idQuat::ToMat3's rows, i.e. the
    /// conjugate of the md6 skeleton's (glam) quaternion convention (see rancher fx.rs ViewTags).
    pub rot: [f32; 4],
    pub parent: String,
}

/// prop name -> tags
pub type Props = HashMap<String, Vec<Tag>>;

fn nums(s: &str) -> Vec<f32> {
    s.trim().trim_start_matches('(').trim_end_matches(')').split_whitespace().filter_map(|n| n.parse().ok()).collect()
}

fn quoted(s: &str) -> Option<String> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(s[a + 1..a + 1 + b].to_string())
}

/// Reads the props of `generated/decls/md6def/<name>.decl` (name like `zion/objects/weapons/shotguns/shotgun.md6`).
pub fn load(c: &Container, name: &str) -> Result<Props> {
    load_depth(c, name, 0)
}

fn load_depth(c: &Container, name: &str, depth: u32) -> Result<Props> {
    ensure!(depth < 16, "md6Def inheritance too deep at {name}");
    let path = format!("generated/decls/md6def/{name}.decl");
    let text = String::from_utf8_lossy(&c.read_by_name(&path).with_context(|| path.clone())?).into_owned();
    let mut props = Props::new();
    let mut in_props = false;
    let mut depth_props = 0i32;
    let mut prop: Option<String> = None;
    let mut tag: Option<Tag> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("inherit ") {
            if let Some(parent) = quoted(rest).or_else(|| Some(rest.trim().to_string())) {
                if let Ok(p) = load_depth(c, &parent, depth + 1) {
                    for (k, v) in p {
                        props.entry(k).or_default().extend(v);
                    }
                }
            }
            continue;
        }
        if !in_props {
            if l == "props {" || l == "props" {
                in_props = true;
                depth_props = if l.ends_with('{') { 1 } else { 0 };
            }
            continue;
        }
        let opens = l.matches('{').count() as i32;
        let closes = l.matches('}').count() as i32;
        if let Some(rest) = l.strip_prefix("prop ") {
            prop = quoted(rest);
        } else if let Some(rest) = l.strip_prefix("tag ") {
            tag = quoted(rest).map(|n| Tag { name: n, trans: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], parent: String::new() });
        } else if let Some(rest) = l.strip_prefix("trans ") {
            if let Some(t) = tag.as_mut() {
                let v = nums(rest);
                if v.len() == 3 {
                    t.trans = [v[0], v[1], v[2]];
                }
            }
        } else if let Some(rest) = l.strip_prefix("rot ") {
            if let Some(t) = tag.as_mut() {
                let v = nums(rest);
                if v.len() == 4 {
                    t.rot = [v[0], v[1], v[2], v[3]];
                }
            }
        } else if let Some(rest) = l.strip_prefix("parent ") {
            if let Some(t) = tag.as_mut() {
                t.parent = quoted(rest).unwrap_or_else(|| rest.trim().to_string());
            }
        }
        depth_props += opens - closes;
        if closes > 0 {
            if let Some(t) = tag.take() {
                if let Some(p) = &prop {
                    props.entry(p.clone()).or_default().push(t);
                }
            }
        }
        if depth_props <= 0 {
            break;
        }
    }
    Ok(props)
}
