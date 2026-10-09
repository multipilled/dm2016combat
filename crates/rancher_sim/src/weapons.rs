//! Player weapons: definitions read from the install's decls (weapon -> ammo -> projectile -> damage)
//! and the firing simulation, decoded from DOOMx64.exe (notes and addresses: gamedata/re/WEAPONS.md).
//!
//! Decoded: fire gate and refire timing (CanFire / FinishFire / GetFiringInterval), single-tap latch and
//! shot queueing, ammo use, heat/glow/overheat, chaingun barrel spin and progressive firing interval,
//! weapon change timing, view kick (new axis system and the old BFG system), spread base/movement/aim
//! terms and per-shot growth/return, shot directions (rings, gaussian), damage falloff and radius damage.
//! Two drivers: `Arsenal::tick` (sim-timed: fire on the first frame CanFire passes, ready when the bring-up
//! duration ends) and `hands::Hands` (the decoded idHands state machine: fires on the anim web's
//! ae_fireWeaponRight events; gamedata/re/HANDS.md).
//! Mods (gamedata/re/MODS.md): `mods` (perk / upgrade data, ApplyUpgradeModifier slots), `charge` (fire modes,
//! charge state machine, bursts, fire gate), `modswitch` (perk switcher); the hands drive trigger mode 7.
//! NOT YET: chainsaw fuel use, aim assist, the weapon-specific mod systems listed in UNVERIFIED.md.

use std::sync::Arc;

use glam::Vec3;

pub mod arsenal;
pub mod charge;
pub mod damage;
pub mod decl;
pub mod detonate;
pub mod hands;
pub mod interp;
pub mod kick;
pub mod melee;
pub mod mods;
pub mod modswitch;
pub mod railgun;
pub mod seek;
pub mod select;
pub mod spread;
pub mod targeting;
pub mod zoom;

pub use arsenal::{AmmoPool, Arsenal, Barrel, BarrelState, KickState, Shot, WeaponEvent, WeaponInput, WeaponPhase, WeaponState};
pub use damage::DamageDef;
pub use charge::{Charge, ChargeEvent, ChargeState, ModState};
pub use mods::{Applied, ModLoadout, PerkFamily, WeaponMods};
pub use modswitch::{ModSwitch, ModSwitchEvent};
pub use railgun::RailGunState;
pub use decl::{load_arsenal, BurstInfo, ChaingunData, RailGunData, ChargeInfo, ChargeItem, ChargeProperty, HandsDecl, HeatInfo, ProjectileDef, TargetLockData, ExplodeProjectiles, WeaponDef, SP_WEAPONS};
pub use hands::{Hands, HandsAction, HandsInput, HandsScalars, HandsState, HandsWeb, LandSize, StateEvent, WebEvent, WebRequest};
pub use kick::{FeedBack, KickAxis, KickComponent, OldKick, ViewKick, WeaponKick};
pub use melee::{MeleeBounds, MeleeDamageType, MeleeDecl, MeleeHit, MeleeProjectile, MeleeTrace, SweepHit};
pub use select::{SelectInput, WeaponSelect};
pub use spread::{rand_gaussian, shot_direction, PlayerSpread, SpreadInput, SpreadParams, SpreadPattern};
pub use zoom::{Interp, Zoom, ZoomInfo, ZoomInput, ZoomMode};
pub use seek::SeekParms;
pub use targeting::{LockTarget, TargetEvent, TargetSlot, TargetState, TargetView, TargetWorld, Targeting};

/// The game's LCG (gameLocal+0x285be8): `seed * 0x19660d + 0x3c6ef35f`, 15-bit draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRng(pub u32);

impl GameRng {
    fn step(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(0x19660d).wrapping_add(0x3c6ef35f);
        (self.0 >> 10) & 0x7fff
    }

    /// `((seed >> 10) & 0x7fff) * 3.051851e-05`.
    pub fn next01(&mut self) -> f32 {
        self.step() as f32 * 3.051851e-05
    }

    /// `(seed >> 10) & 0x7fff` (integer draws such as addedFiringInterval).
    pub fn next15(&mut self) -> u32 {
        self.step()
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub point: Vec3,
    pub normal: Vec3,
    pub target: Option<usize>,
    pub damage: f32,
}

/// A projectile in flight (rocket, plasma, BFG ball).
#[derive(Debug, Clone)]
pub struct Projectile {
    pub def: Arc<WeaponDef>,
    pub pos: Vec3,
    pub vel: Vec3,
    pub age_ms: f32,
    /// Weapon (arsenal index) and the shot (Fired event) it belongs to.
    pub weapon: usize,
    pub shot: u32,
    /// The shot's use-time damage scale (Shot::damage_scale).
    pub damage_scale: f32,
    /// Game side id (the weapon's launched list, weapons::detonate).
    pub id: u32,
    /// The seek (weapons::seek) when the projectile entity can seek and the shot had a target.
    pub seek: Option<seek::Seeker>,
}
