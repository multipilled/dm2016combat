//! DOOM (2016) binary SWF GUIs (`generated/swf/*.bswf`), read from the user's own install.

pub mod as2;
pub mod assets;
pub mod bswf;
pub mod font;
pub mod hud;
pub mod menu;
pub mod placement;
pub mod player;
pub mod raster;
pub mod render;
pub mod tags;
pub mod texture;
pub mod wheel;
mod vm;

pub use assets::Assets;
pub use bswf::{DictEntry, Matrix, Rect, Sprite, Swf};
pub use player::{ObjId, Player, Value};

/// Writes straight-alpha RGBA8 as a PNG.
pub fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// The runtime lives in ECS resources, so it must stay thread-safe.
#[allow(dead_code)]
fn assert_send_sync() {
    fn s<T: Send + Sync>() {}
    s::<Player>();
    s::<hud::Hud>();
    s::<render::DrawList>();
}
