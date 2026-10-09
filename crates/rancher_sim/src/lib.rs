//! Headless, deterministic reimplementation of DOOM (2016) player movement.
//!
//! Every tunable comes from the user's own install through [`config::MoveConfig`]; the algorithms
//! mirror the game's player physics code. Nothing here depends on rendering, so movement can be
//! checked frame by frame in tests.

pub mod ai;
pub mod animweb;
pub mod cmd;
pub mod collision;
pub mod config;
pub mod demons;
pub mod handlayers;
pub mod install;
pub mod physics;
pub mod pickups;
pub mod player;
pub mod upgrades;
pub mod viewfx;
pub mod weapons;

pub use glam::Vec3;
