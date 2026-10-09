//! Decl lookup with `inherit` resolution, reading `generated/decls/<type>/<name>.decl` on demand.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

use crate::Container;
use crate::decl::{self, Block};

pub struct DeclDb {
    container: Arc<Container>,
    cache: Mutex<HashMap<String, Arc<Block>>>,
}

impl DeclDb {
    pub fn new(container: Arc<Container>) -> Self {
        Self { container, cache: Mutex::new(HashMap::new()) }
    }

    pub fn container(&self) -> &Container {
        &self.container
    }

    pub fn container_arc(&self) -> Arc<Container> {
        self.container.clone()
    }

    /// `kind` is the decl folder (e.g. "weapon", "ammo", "projectile", "damage", "jumpboots");
    /// `name` is the decl name as referenced by other decls (e.g. "weapon/zion/player/sp/shotgun").
    pub fn get(&self, kind: &str, name: &str) -> Result<Arc<Block>> {
        let key = format!("{kind}/{name}");
        if let Some(b) = self.cache.lock().unwrap().get(&key) {
            return Ok(b.clone());
        }
        let resolved = Arc::new(self.resolve(kind, name, 0)?);
        self.cache.lock().unwrap().insert(key, resolved.clone());
        Ok(resolved)
    }

    pub fn raw(&self, kind: &str, name: &str) -> Result<Block> {
        let path = format!("generated/decls/{kind}/{name}.decl");
        let bytes = self.container.read_by_name(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        decl::parse(&text).with_context(|| format!("parsing {path}"))
    }

    fn resolve(&self, kind: &str, name: &str, depth: u32) -> Result<Block> {
        if depth > 32 {
            bail!("inheritance too deep at {kind}/{name}");
        }
        let own = self.raw(kind, name)?;
        match own.str("inherit") {
            Some(parent) if !parent.is_empty() => {
                let base = self.resolve(kind, parent, depth + 1)?;
                Ok(base.merged_with(&own))
            }
            _ => Ok(own),
        }
    }
}
