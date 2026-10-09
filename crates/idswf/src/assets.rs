//! Loading SWFs, their atlases, SDF fonts, strings and material textures from the user's install.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use idres::Container;

use crate::bswf::{DictEntry, Swf};
use crate::font::{self, SdfFont};
use crate::player::{self, Player};
use crate::texture::{self, Texture};

pub struct Assets {
    pub container: Arc<Container>,
    fonts: Mutex<HashMap<String, Arc<SdfFont>>>,
    materials: Mutex<HashMap<String, Option<Arc<Texture>>>>,
    pub strings: Arc<HashMap<String, String>>,
}

impl Assets {
    pub fn new(container: Arc<Container>) -> Assets {
        let strings = player::load_strings(&container).unwrap_or_default();
        Assets { container, fonts: Mutex::new(HashMap::new()), materials: Mutex::new(HashMap::new()), strings: Arc::new(strings) }
    }

    pub fn open(doom_dir: &std::path::Path) -> Result<Assets> {
        let c = Container::open(&doom_dir.join("base"), "gameresources")?;
        Ok(Assets::new(Arc::new(c)))
    }

    pub fn swf(&self, name: &str) -> Result<Arc<Swf>> {
        let path = if name.starts_with("generated/") { name.to_string() } else { format!("generated/swf/{name}.bswf") };
        let bytes = self.container.read_by_name(&path)?;
        Ok(Arc::new(Swf::parse(&bytes).with_context(|| format!("parsing {path}"))?))
    }

    pub fn atlas(&self, name: &str) -> Result<Arc<Texture>> {
        let path = format!("generated/swf/{name}.bimage");
        Ok(Arc::new(texture::decode_bimage(&self.container.read_by_name(&path)?)?))
    }

    pub fn font(&self, face: &str) -> Result<Arc<SdfFont>> {
        let key = font::face_dir(face);
        if let Some(f) = self.fonts.lock().unwrap().get(&key) {
            return Ok(f.clone());
        }
        let f = Arc::new(SdfFont::load(&self.container, face)?);
        self.fonts.lock().unwrap().insert(key, f.clone());
        Ok(f)
    }

    /// A player for `generated/swf/<name>.bswf` with its atlas, fonts and strings attached.
    pub fn player(&self, name: &str) -> Result<Player> {
        let swf = self.swf(name)?;
        let atlas = self.atlas(name).ok();
        let mut fonts = HashMap::new();
        for e in &swf.dict {
            if let DictEntry::Font(f) = e {
                match self.font(&f.name) {
                    Ok(sf) => {
                        fonts.insert(font::face_dir(&f.name), sf);
                    }
                    Err(e) => eprintln!("{name}: font '{}': {e:#}", f.name),
                }
            }
        }
        let mut p = Player::new_with(swf, atlas, fonts, Some(self.strings.clone()));
        p.name = name.to_string();
        Ok(p)
    }

    /// The first image stage of a material decl (`generated/decls/material/<name>.decl`), decoded.
    pub fn material_texture(&self, material: &str) -> Option<Arc<Texture>> {
        if let Some(t) = self.materials.lock().unwrap().get(material) {
            return t.clone();
        }
        let t = self.load_material(material).ok().map(Arc::new);
        self.materials.lock().unwrap().insert(material.to_string(), t.clone());
        t
    }

    fn load_material(&self, material: &str) -> Result<Texture> {
        let decl = self.container.read_by_name(&format!("generated/decls/material/{material}.decl"))?;
        let text = String::from_utf8_lossy(&decl);
        // GUI materials name their image in a "map"/"transmap"/"diffusemap" line.
        let mut image = None;
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
            if matches!(k.to_ascii_lowercase().as_str(), "transmap" | "map" | "diffusemap" | "albedomap" | "colormap") && !v.starts_with('_') {
                image = Some(v.trim_matches('"').to_string());
                break;
            }
        }
        let image = image.with_context(|| format!("no image in material {material}"))?;
        let image = [".tga", ".png", ".jpg", ".dds"].iter().fold(image, |s, ext| s.strip_suffix(ext).map(str::to_string).unwrap_or(s));
        let candidates = [format!("generated/image/{image}.bimage"), format!("generated/image/{image}$mipmaps.bimage")];
        for c in &candidates {
            if let Ok(b) = self.container.read_by_name(c) {
                return texture::decode_bimage(&b);
            }
        }
        // Image names can carry option suffixes ("$borderclamp"...); find any match.
        let prefix = format!("generated/image/{image}");
        let e = self
            .container
            .live_entries()
            .find(|e| e.full_name.starts_with(&prefix) && e.full_name.ends_with(".bimage"))
            .with_context(|| format!("image {image} for material {material}"))?;
        texture::decode_bimage(&self.container.read(e)?)
    }
}
