//! Demon projectiles (the Imp's fireball): the projectile decl chain the AI's ammo item names, the projectile
//! entity's flight and its impact on the player (gamedata/re/DEMONS.md section 13).
//!
//! Chain: AI inventory `ammo/zion/ai/imp/fireball` -> projectile decl `projectile/zion/ai/imp/fireball`
//! (notHitscanInfo: speed 1250, parabolicFlight, maxTrajectoryTime 1, damageDecl damage/zion/ai/imp/fireball)
//! -> entityDef `projectile_ent/zion/ai/imp/fireball` (class idProjectile_Rocket, clip box 15 x 15 x 15 offset z 2,
//! explodeOnImpact). The damage reaches the player through idPlayer::Damage ([`super::live::player_damage`]):
//! fireball 60 x playerDamageScale 0.25 = 15 on Hurt Me Plenty.

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decldb::DeclDb;

use crate::collision::{Hull, World};

/// idDeclProjectile + its notHitscanInfo_t fields the AI throw and the flight read (reflection
/// idDeclProjectile::notHitscanInfo_t: +0x110 parabolicFlight, +0x114 minTrajectoryTime, +0x118 maxTrajectoryTime;
/// physicsProperties_t +0x1c noGravity).
#[derive(Debug, Clone, PartialEq)]
pub struct AiProjectileDef {
    pub name: String,
    /// notHitscanInfo.entityDef.
    pub entity_def: String,
    /// notHitscanInfo.speed (units/s).
    pub speed: f32,
    /// !notHitscanInfo.physicsProperties.noGravity.
    pub gravity: bool,
    /// notHitscanInfo.parabolicFlight: "this projectile's flight path is a parabola".
    pub parabolic: bool,
    /// notHitscanInfo.min/maxTrajectoryTime: "time in flight in seconds (used to calculate trajectories)".
    pub min_trajectory_time: f32,
    pub max_trajectory_time: f32,
    pub explode_on_impact: bool,
    pub damage_decl: String,
    /// The projectile entity's clipModelInfo size (x, y, z) and offset.
    pub clip_size: Vec3,
    pub clip_offset: Vec3,
    /// idDeclProjectile +0x398 maxRange / +0x39c minRange ("minimum useful range of the projectile (for AI)"),
    /// ints; ctor 0x1406ec0e0 defaults 8192 / 0 (asm 0x1406ec399 stores the qword 0x2000).
    pub max_range: f32,
    pub min_range: f32,
}

impl AiProjectileDef {
    /// From an ammo decl (`ammo/zion/ai/imp/fireball`: its projectileDecl) or a projectile decl name.
    pub fn load(db: &DeclDb, name: &str) -> Result<AiProjectileDef> {
        let proj = match db.get("ammo", name) {
            Ok(a) => a.str("edit.projectileDecl").map(str::to_string).with_context(|| format!("ammo {name} without projectileDecl"))?,
            Err(_) => name.to_string(),
        };
        let p = db.get("projectile", &proj).with_context(|| format!("projectile {proj}"))?;
        let e = p.block("edit").context("projectile without edit")?;
        let entity_def = e.str("notHitscanInfo.entityDef").unwrap_or("").to_string();
        let ent = db.get("entitydef", &entity_def).ok();
        let ee = ent.as_ref().and_then(|b| b.block("edit"));
        let v3 = |k: &str| Vec3::new(
            ee.and_then(|b| b.f32(&format!("{k}.x"))).unwrap_or(0.0),
            ee.and_then(|b| b.f32(&format!("{k}.y"))).unwrap_or(0.0),
            ee.and_then(|b| b.f32(&format!("{k}.z"))).unwrap_or(0.0),
        );
        let flag = |k: &str, d: bool| e.path(k).and_then(|v| v.as_bool()).unwrap_or(d);
        Ok(AiProjectileDef {
            name: proj.clone(),
            speed: e.f32("notHitscanInfo.speed").unwrap_or(0.0),
            gravity: !flag("notHitscanInfo.physicsProperties.noGravity", false),
            parabolic: flag("notHitscanInfo.parabolicFlight", false),
            min_trajectory_time: e.f32("notHitscanInfo.minTrajectoryTime").unwrap_or(0.0),
            max_trajectory_time: e.f32("notHitscanInfo.maxTrajectoryTime").unwrap_or(0.0),
            explode_on_impact: flag("notHitscanInfo.explodeOnImpact", false) || ee.and_then(|b| b.path("explodeOnImpact")).and_then(|v| v.as_bool()).unwrap_or(false),
            damage_decl: e.str("damageDecl").unwrap_or("").to_string(),
            clip_size: v3("clipModelInfo.size"),
            clip_offset: v3("clipModelInfo.offset"),
            max_range: e.f32("maxRange").unwrap_or(8192.0),
            min_range: e.f32("minRange").unwrap_or(0.0),
            entity_def,
        })
    }

    /// The clip model as a box centred on the origin + offset.
    pub fn hull(&self) -> Hull {
        let h = self.clip_size * 0.5;
        Hull::cuboid(self.clip_offset - h, self.clip_offset + h)
    }
}

/// What a flight step ran into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Impact {
    /// The player's clip box.
    Player { at: Vec3 },
    /// World geometry.
    World { at: Vec3, normal: Vec3 },
}

/// A projectile in flight.
#[derive(Debug, Clone)]
pub struct AiProjectile {
    pub def: std::sync::Arc<AiProjectileDef>,
    pub pos: Vec3,
    pub vel: Vec3,
    /// Seconds since launch.
    pub age: f32,
    /// Who threw it (demon id) and from where (the damage falloff distance).
    pub owner: u32,
    pub launch: Vec3,
}

/// INTERIM lifetime cap of a projectile that never hits anything (the entity pool / range removal was not traced).
pub const MAX_FLIGHT_SECS_INTERIM: f32 = 10.0;

impl AiProjectile {
    /// One flight step of `dt` seconds under `gravity` (units/s^2, applied when the decl has gravity): the clip box
    /// swept through the world, and the swept box against the player's clip box `player` (lo, hi).
    /// INTERIM: idProjectile's physics (rigid body / simple physics with the decl's friction and bounciness) is
    /// not decoded; explodeOnImpact projectiles stop at the first contact, so a swept box with explicit-Euler
    /// gravity is used.
    pub fn step(&mut self, world: &World, gravity: f32, player: (Vec3, Vec3), dt: f32) -> Option<Impact> {
        let mut vel = self.vel;
        if self.def.gravity {
            vel.z -= gravity * dt;
        }
        let end = self.pos + vel * dt;
        let h = self.def.clip_size * 0.5;
        let r = h.max_element();
        let c = self.def.clip_offset;
        let tw = world.translate(&self.def.hull(), self.pos, end);
        let tp = super::live::sweep_hits_box(self.pos + c, end + c, r, player.0, player.1);
        self.age += dt;
        match (tw.hit().then_some(tw.fraction), tp) {
            (Some(fw), Some(fp)) if fp <= fw => return Some(Impact::Player { at: self.pos + (end - self.pos) * fp }),
            (None, Some(fp)) => return Some(Impact::Player { at: self.pos + (end - self.pos) * fp }),
            (Some(_), _) => return Some(Impact::World { at: tw.endpos, normal: tw.c.normal }),
            (None, None) => {}
        }
        self.pos = end;
        self.vel = vel;
        None
    }
}
