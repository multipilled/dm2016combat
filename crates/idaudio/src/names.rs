//! Original source-file names of media, from `soundbanksinfo.xml` (`<File Id="…" Language="…">`
//! `<ShortName>weapons\shotgun\…wav</ShortName>`). Only used for labelling; nothing needs it to play.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

/// media id → (language, original `.wav` short name). The first listing of an id wins.
pub fn media_short_names(xml_path: &Path) -> Result<HashMap<u32, (String, String)>> {
    let text = std::fs::read_to_string(xml_path).with_context(|| format!("reading {}", xml_path.display()))?;
    let mut out = HashMap::new();
    let mut rest = text.as_str();
    while let Some(p) = rest.find("<File Id=\"") {
        rest = &rest[p + 10..];
        let Some(q) = rest.find('"') else { break };
        let id: u32 = match rest[..q].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let head_end = rest.find('>').unwrap_or(0);
        let head = &rest[..head_end];
        if head.ends_with('/') {
            continue; // <File Id="…"/> reference
        }
        let lang = head.find("Language=\"").map_or(String::new(), |l| {
            let s = &head[l + 10..];
            s[..s.find('"').unwrap_or(0)].to_owned()
        });
        let Some(s) = rest.find("<ShortName>") else { break };
        let Some(e) = rest.find("</ShortName>") else { break };
        if e > s {
            out.entry(id).or_insert((lang, rest[s + 11..e].to_owned()));
        }
    }
    Ok(out)
}
