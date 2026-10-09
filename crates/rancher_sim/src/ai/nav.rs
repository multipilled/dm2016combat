//! AAS2 navigation for the headless AI: which area a point is in (the file's BSP tree), a route of reachabilities
//! between areas and the walk path through their start / end points.
//!
//! Format and loader: idres::aas (idAAS2File::LoadBinary 0x141606720). The game routes with idAAS2Local's cluster /
//! portal travel-time caches (WalkPathToGoal 0x1419db420, AAS2_routing.cpp), which are not ported: the route here
//! is INTERIM (shortest total cost over the reachability graph, cost = the reachability's stored travelTime plus
//! the walk to its start at the file's groundSpeed in the travel-time unit, 1/100 s as in id's AAS); UNVERIFIED.md.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::Container;
use idres::aas::Aas;

pub struct Nav {
    pub aas: Aas,
}

fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::from_array(a)
}

#[derive(Copy, Clone, PartialEq)]
struct Item {
    cost: f32,
    area: usize,
}
impl Eq for Item {}
impl Ord for Item {
    fn cmp(&self, o: &Self) -> Ordering {
        o.cost.partial_cmp(&self.cost).unwrap_or(Ordering::Equal).then_with(|| o.area.cmp(&self.area))
    }
}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Nav {
    /// `map` like "game/sp/intro/intro", `aas` like "aas_monster48" (aiConstants.movement.aasName).
    pub fn load(c: &Container, map: &str, aas: &str) -> Result<Nav> {
        let ext = aas.strip_prefix("aas").unwrap_or(aas);
        let leaf = map.rsplit('/').next().unwrap_or(map);
        let path = format!("generated/maps/{map}/{leaf}.baas{ext}");
        let bytes = c.read_by_name(&path).with_context(|| format!("AAS {path}"))?;
        Ok(Nav { aas: Aas::parse(&bytes).with_context(|| format!("parsing {path}"))? })
    }

    /// The area containing `p`: descend the BSP tree from the tree's head node; a node's plane distance
    /// (a*x + b*y + c*z + d) > 0 takes children[0], else children[1]; 0 = solid, negative = -(area).
    /// Side convention as the exe's bounds-in-tree walk 0x1419b4440 (tree headNode from trees[i] + 0xc, plane side
    /// 0x140287a70 with epsilon 0.1: front -> children[0], back -> children[1], crossing -> both); the install test
    /// checks the found area's bounds contain the point. The exe's single-point query itself was not located.
    pub fn point_area(&self, p: Vec3) -> Option<usize> {
        for t in &self.aas.trees {
            let mut n = t.head_node;
            for _ in 0..256 {
                if n <= 0 {
                    break;
                }
                let node = self.aas.nodes.get(n as usize)?;
                let pl = self.aas.planes.get(node.plane as usize)?;
                let d = v3(pl.normal).dot(p) + pl.d;
                n = if d > 0.0 { node.children[0] } else { node.children[1] };
            }
            if n < 0 {
                let a = (-n) as usize;
                if a >= t.first_area as usize && a <= t.last_area as usize {
                    return Some(a);
                }
            }
        }
        None
    }

    /// Whether `p` lies inside `area`'s stored bounds (xy), with `slack` units.
    pub fn in_bounds(&self, area: usize, p: Vec3, slack: f32) -> bool {
        let Some(b) = self.aas.area_bounds.get(area) else { return false };
        p.x >= b[0][0] as f32 - slack && p.x <= b[1][0] as f32 + slack && p.y >= b[0][1] as f32 - slack && p.y <= b[1][1] as f32 + slack
    }

    /// Reachability indices from `from` to `to` (INTERIM routing, see the module docs). None when unreachable.
    pub fn route(&self, from: usize, from_pos: Vec3, to: usize) -> Option<Vec<usize>> {
        let n = self.aas.areas.len();
        if from >= n || to >= n {
            return None;
        }
        if from == to {
            return Some(Vec::new());
        }
        // travel time unit: 1/100 s; groundSpeed in units per second (idAAS2Settings).
        let speed = self.aas.settings.ground_speed().max(1.0);
        let mut best = vec![f32::INFINITY; n];
        let mut via: Vec<Option<usize>> = vec![None; n];
        let mut at = vec![Vec3::ZERO; n];
        let mut heap = BinaryHeap::new();
        best[from] = 0.0;
        at[from] = from_pos;
        heap.push(Item { cost: 0.0, area: from });
        while let Some(Item { cost, area }) = heap.pop() {
            if area == to {
                break;
            }
            if cost > best[area] {
                continue;
            }
            for (ri, r) in self.aas.reach_from(area) {
                let next = r.to_area as usize;
                if next >= n {
                    continue;
                }
                let walk = at[area].distance(v3(r.start())) * 100.0 / speed;
                let c = cost + walk + r.travel_time as f32;
                if c < best[next] {
                    best[next] = c;
                    via[next] = Some(ri);
                    at[next] = v3(r.end());
                    heap.push(Item { cost: c, area: next });
                }
            }
        }
        if !best[to].is_finite() {
            return None;
        }
        let mut out = Vec::new();
        let mut a = to;
        while a != from {
            let ri = via[a]?;
            out.push(ri);
            a = self.aas.reach[ri].from_area as usize;
        }
        out.reverse();
        Some(out)
    }

    /// Waypoints from `from` to `to`: each reachability's start and end, then `to` (empty when no route).
    pub fn path(&self, from: Vec3, to: Vec3) -> Vec<Vec3> {
        let (Some(a), Some(b)) = (self.point_area(from), self.point_area(to)) else { return Vec::new() };
        let Some(r) = self.route(a, from, b) else { return Vec::new() };
        let mut pts = Vec::with_capacity(r.len() * 2 + 1);
        for ri in r {
            let rc = &self.aas.reach[ri];
            for p in [v3(rc.start()), v3(rc.end())] {
                if pts.last().is_none_or(|q: &Vec3| q.distance(p) > 1.0) {
                    pts.push(p);
                }
            }
        }
        pts.push(to);
        pts
    }
}
