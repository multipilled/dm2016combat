//! Demons (idAI2) as damageable targets: decls read from the install, the engine's damage chain, the leaky pain
//! buckets and SDPS pain reactions, hit spheres, and the pain / stagger / death states that drive the demon's
//! anim web. Decoded from DOOMx64.exe; notes and addresses: gamedata/re/DEMONS.md.
//!
//! First demon: the Possessed (`ai/zombie/scientist`). Decoded and followed: Damage_Calculate's scaling and
//! per-location damage (ApplyLocationDamage), the kill rule, leaky buckets, the SDPS reaction search, stagger
//! lengths and recovery health, death tags. INTERIM / not decoded: navigation and AI behaviour (the demon
//! stands), CheckPainAnim's full pain-graph walk (stationary nodes only, see `pain`), gore / dismemberment and
//! injured states, glory kills, the deferred fire manager's batching of pellet traces (one event per trace).

pub mod actor;
pub mod damage;
pub mod decl;
pub mod glory;
pub mod hit;
pub mod live;
pub mod pain;
pub mod repulsor;
pub mod projectile;

pub use actor::{Demon, DemonEvent, DemonPhase, WebRequest};
pub use damage::{AiDamageParms, DamageEvent, DamageResult, TraceHit};
pub use decl::{DemonDef, JointGroups, WebTags};
pub use hit::{HitSphere, SphereHit};
pub use pain::{LeakyBucket, PainType};

/// The Possessed: the campaign's first and most common low-tier demon.
pub const POSSESSED: &str = "ai/zombie/scientist";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The Possessed's decls from the user's install (skipped without one).
    #[test]
    fn install_possessed() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let def = DemonDef::load(&inst.decls, POSSESSED).unwrap();
        assert_eq!(def.health, 150.0);
        assert_eq!(def.anim_web, "zion/characters/monsters/zombie");
        assert_eq!(def.pain_info.max, 150.0);
        let names: Vec<&str> = def.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["head", "left_arm", "right_arm", "chest", "left_leg", "right_leg", "gut"]);
        let head = &def.groups[0];
        let pistol = head.scalars.iter().find(|s| s.decl.as_deref() == Some("damage/zion/firearm/sp/pistol")).unwrap();
        assert_eq!(pistol.damage_scale, 7.5);
        // zion/ai/default's head list has 5 entries, so the zombie's pistol entry [5] carries no headShot flag.
        assert!(!pistol.head_shot);
        assert_eq!(def.damage_group_of_joint("jaw"), Some(0));
        assert_eq!(def.damage_group_of_joint("Hips"), Some(6));
        assert_eq!(def.behaviors.stagger_vulnerable_ms, 5000);
        assert_eq!(def.behaviors.monster_type, 2);
        assert!(def.threat.use_full_search);
        assert_eq!(def.threat.mappings.len(), 23);
        let d: Vec<_> = def.threat.default.iter().map(|r| (r.reaction, r.prerequisite_health_fraction, r.total_health_fraction)).collect();
        assert_eq!(d, [(PainType::StaggerVulnerable, 0.6, 0.0), (PainType::Falter, 1.0, 0.3)]);
        assert!(def.pain_graph.subgraph(PainType::Falter).is_some());
        assert!(def.pain_graph.subgraph(PainType::StaggerVulnerable).is_some());
        let tags = WebTags::load(inst.decls.container(), &def.anim_web).unwrap();
        let death = tags.node("death", "death").unwrap();
        assert_eq!(death.groups.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(), ["type", "direction", "part", "startingInjury", "source"]);
        assert_eq!(death.aliases[0], ["moving", "heavy", "front", "not_injured"]);
        // Gore wound mesh kits start hidden (13 wound meshes on zombie_scientist.bmd6model).
        let hidden = def.wound_meshes();
        assert!(hidden.contains(&"wound_chest_venti".to_string()) && hidden.contains(&"wound_neck".to_string()), "{hidden:?}");
        assert!(!hidden.contains(&"zombie_scientist".to_string()));

        // Damage: a pistol body shot does 20, a headshot 150 (kills).
        let parms = Arc::new(AiDamageParms::from_decl(&inst.decls, "damage/zion/firearm/sp/pistol").unwrap());
        let shot = |j: &str| DamageEvent {
            decl: parms.name.clone(),
            traces: vec![TraceHit { joint: Some(j.into()), point: glam::Vec3::ZERO }],
            attacker_origin: Some(glam::Vec3::new(512.0, 0.0, 0.0)),
            scale: 1.0,
            dir: glam::Vec3::new(-1.0, 0.0, 0.0),
            splash_fraction: -1.0,
        };

        // Three pistol body shots: the bucket passes 45 when health reaches 60% -> STAGGER_VULNERABLE wins
        // over FALTER; a standing zombie hit from the front plays stagger/hands_front_into.
        let mut z = Demon::new(def.clone(), tags.clone(), glam::Vec3::ZERO, 0.0, 1, actor::AiCvars::default(), 1);
        let mut pains = Vec::new();
        for i in 0..3 {
            let t = 1000 + i * 100;
            z.damage(&shot("spine2"), parms.clone(), t).unwrap();
            pains.extend(z.update(t, Some(("hands_combat", "idle"))).into_iter().filter(|e| matches!(e, DemonEvent::Pain { .. })));
        }
        assert_eq!(pains, [DemonEvent::Pain { reaction: PainType::StaggerVulnerable, sub: "stagger".into(), state: "hands_front_into".into() }]);
        assert!(matches!(z.phase, DemonPhase::Stagger { vulnerable: true, until: 6000, .. }), "{:?}", z.phase);
        // A 50-damage chest hit on a full-health zombie: bucket 50 > 45, health 66% -> FALTER, chest anim.
        let mut z = Demon::new(def.clone(), tags.clone(), glam::Vec3::ZERO, 0.0, 1, actor::AiCvars::default(), 1);
        z.damage(&DamageEvent { scale: 2.5, ..shot("spine2") }, parms.clone(), 1000).unwrap();
        let ev = z.update(1000, Some(("hands_combat", "idle")));
        assert!(ev.contains(&DemonEvent::Pain { reaction: PainType::Falter, sub: "falter".into(), state: "hands_front".into() }), "{ev:?}");
        let chest = tags.aliases("falter", "hands_front").unwrap().iter().position(|t| t.iter().any(|x| x == "chest")).unwrap();
        assert!(z.requests.contains(&WebRequest::Scalar { name: "painIndex".into(), value: chest as f32 }), "{:?}", z.requests);
        let mut z = Demon::new(def.clone(), tags.clone(), glam::Vec3::ZERO, 0.0, 1, actor::AiCvars::default(), 1);
        let r = z.damage(&shot("spine2"), parms.clone(), 1000).unwrap();
        assert_eq!(r.health, 20.0);
        assert_eq!(z.health, 130.0);
        z.update(1000, Some(("hands_combat", "idle")));
        let r = z.damage(&shot("head"), parms.clone(), 2000).unwrap();
        assert_eq!(r.health, 150.0);
        assert!(r.killed);
        let ev = z.update(2000, Some(("hands_combat", "idle")));
        assert!(ev.iter().any(|e| matches!(e, DemonEvent::Death { tags } if tags.contains(&"head".to_string()) && tags.contains(&"front".to_string()))));
    }
}

#[cfg(test)]
mod dump {
    use super::*;

    /// RE helper: the death anims' root / hips / leg-rig tracks (`cargo test --release -p rancher_sim dump_death -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn dump_death_anims() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let def = DemonDef::load(&inst.decls, POSSESSED).unwrap();
        let c = inst.decls.container();
        let src = String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/animweb/{}.decl", def.anim_web)).unwrap()).into_owned();
        let web = idres::animweb::AnimWeb::parse(&src).unwrap();
        let mut printed = false;
        let filter = std::env::var("DUMP_SUB").unwrap_or_else(|_| "death".into());
        let mut skel: Option<idres::md6::Md6Skel> = None;
        for sw in web.sub_webs.iter().filter(|s| filter.split(',').any(|f| f == s.name)) {
            for n in &sw.nodes {
                println!("node {}/{} delta {:?} flags {:?}", sw.name, n.state, n.delta, n.custom_flags);
                for t in &n.trees {
                    for a in &t.anims {
                        let Ok(b) = c.read_by_name(&idres::animweb::anim_resource(&a.name)) else { continue };
                        let an = idres::md6anim::Md6Anim::parse(&b).unwrap();
                        if skel.is_none() {
                            let m = idres::md6::Md6Model::parse(&c.read_by_name("generated/basemodel/md6/characters/monsters/zombie/base/assets/mesh/zombie_scientist.bmd6model").unwrap()).unwrap();
                            skel = c.read_by_name(&format!("generated/skeleton/{}.bmd6skl", m.skeleton.trim_end_matches(".md6skl"))).ok().and_then(|b| idres::md6::Md6Skel::parse(&b).ok());
                        }
                        let Some(sk) = skel.as_ref() else { println!("  {} skeleton {}", a.name, an.skeleton); continue };
                        println!("  {} tags {:?} frames {} rate {} flags {:#x}", a.name, a.tags, an.num_frames, an.frame_rate, an.flags);
                        if std::env::var("DUMP_SKEL").is_ok() && !std::mem::replace(&mut printed, true) {
                            for (j, n) in sk.names.iter().enumerate() {
                                let l = n.to_ascii_lowercase();
                                if l.starts_with("rig_") || l == "leftarm" || l == "leftforearm" || l == "lefthand" || l.starts_with("leftforearmroll") || l.contains("leg") || l.contains("foot") || l.contains("toe") || l == "hips" || l == "origin" {
                                    let keyed = |js: &[u16]| js.contains(&(j as u16));
                                    println!("  skel [{j}] {n} parent {} T {:?} R {:?} anim R{} T{} constR{} constT{}", sk.parents[j], sk.translations[j], sk.rotations[j], keyed(&an.rot.joints), keyed(&an.trans.joints), an.const_r.iter().any(|x| x.0 as usize == j), an.const_t.iter().any(|x| x.0 as usize == j));
                                }
                            }
                        }
                        let want = std::env::var("DUMP_JOINTS").unwrap_or_else(|_| "origin,hips".into());
                        for jn in want.split(',') {
                            let Some(j) = sk.names.iter().position(|x| x.eq_ignore_ascii_case(jn)) else { continue };
                            let f = |fr: f32| {
                                let p = an.sample(fr);
                                let t = p.trans.iter().find(|x| x.0 as usize == j).map(|x| x.1);
                                let r = p.rot.iter().find(|x| x.0 as usize == j).map(|x| x.1);
                                (t, r)
                            };
                            let last = an.num_frames.saturating_sub(1) as f32;
                            let (t0, r0) = f(0.0);
                            let (t1, r1) = f(last);
                            let fmt3 = |t: Option<[f32; 3]>| t.map(|t| format!("({:.1} {:.1} {:.1})", t[0], t[1], t[2])).unwrap_or("-".into());
                            let fmt4 = |t: Option<[f32; 4]>| t.map(|t| format!("({:.3} {:.3} {:.3} {:.3})", t[0], t[1], t[2], t[3])).unwrap_or("-".into());
                            if let Ok(fs) = std::env::var("DUMP_FRAMES") {
                                for fr in fs.split(',').filter_map(|x| x.parse::<f32>().ok()) {
                                    let (t, r) = f(fr);
                                    println!("      f{fr}: T {} R {}", fmt3(t), fmt4(r));
                                }
                            }
                            println!("    {jn}[{j}] T0 {} T1 {} R0 {} R1 {} bindT {:?}", fmt3(t0), fmt3(t1), fmt4(r0), fmt4(r1), sk.translations[j]);
                        }
                    }
                }
            }
        }
    }
}
