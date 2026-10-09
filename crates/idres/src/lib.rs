//! Readers for DOOM (2016) data, operating only on the user's own install.

pub mod aas;
pub mod bcm;
pub mod bimage;
pub mod bmodel;
pub mod container;
pub mod crypt;
pub mod decl;
pub mod decldb;
pub mod entities;
pub mod animweb;
pub mod ldecl;
pub mod md6def;
pub mod exe;
pub mod md6;
pub mod md6anim;
pub mod vt;
pub mod vtex;

pub use container::{Container, Entry};

use std::path::{Path, PathBuf};

/// Steam's default location for DOOM (2016); callers should let users override it.
pub const DEFAULT_STEAM_DIRS: &[&str] = &[
    r"C:\Program Files (x86)\Steam\steamapps\common\DOOM",
    r"C:/Program Files (x86)/Steam/steamapps/common\DOOM",
];

/// Finds a DOOM (2016) install: `DOOM_DIR` env var first, then known Steam library paths.
pub fn find_install() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("DOOM_DIR") {
        let dir = PathBuf::from(dir);
        if is_install(&dir) {
            return Some(dir);
        }
    }
    DEFAULT_STEAM_DIRS.iter().map(PathBuf::from).find(|d| is_install(d))
}

pub fn is_install(dir: &Path) -> bool {
    dir.join("base").join("gameresources.resources").is_file()
}
