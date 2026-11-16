//! Ist die Burg geschlossen? Kann ein Gegner zu Fuß bis an den Bergfried laufen, oder
//! muss er erst durch Mauern und Tore?
//!
//! Geprüft wird am Boden, so wie fremde Einheiten laufen: Gelände, Wasser, Wald und
//! alle Gebäude sperren; eigene Tore gelten als zu (sie schließen, wenn Fremde kommen),
//! außer sie stehen auf „offen“. Kommt man vom Bergfried aus zu einem fremden
//! Startplatz, ist die Burg offen. Die Lücke ist das Feld auf diesem Weg, an dem er
//! zuerst an der eigenen Befestigung vorbeiführt.
//!
//! Dieselbe Prüfung nutzen die Computergegner (Stärke schätzen, Mauer planen) und die
//! Anzeige für Spieler (`ServerMsg::Castle`).

use std::collections::VecDeque;

use protocol::{BuildingCategory, BuildingKind, CastleStatus, GateMode};

use super::{gate::is_gate, gate::passage_tiles, World};

/// Nachbarn in alle acht Richtungen.
const DIRS: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

/// Startplätze näher als so viele Felder am eigenen Bergfried zählen als die eigenen.
const OWN_SPAWN: f32 = 20.0;

impl World {
    /// Stand der Burg dieses Spielers (ohne Bergfried: offen, ohne Mauer).
    pub fn castle_status(&self, player: u32) -> CastleStatus {
        let open = CastleStatus { walled: false, closed: false, gap: None };
        let Some(keep) = self.buildings.iter().find(|b| b.owner == player && b.kind == BuildingKind::Keep) else {
            return open;
        };
        let walled = self.buildings.iter().any(|b| {
            b.owner == player
                && b.kind != BuildingKind::Keep
                && self.economy.building(b.kind).is_some_and(|d| d.category == BuildingCategory::Fortification)
        });
        let Some(start) = keep.entrance else { return CastleStatus { walled, ..open } };
        let center = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        let (w, h) = (self.map.width as i32, self.map.height as i32);
        let idx = |x: i32, y: i32| (y * w + x) as usize;

        // Eigene Tore (nicht „offen“) gelten als zu.
        let mut shut = vec![false; (w * h) as usize];
        for b in self.buildings.iter().filter(|b| b.owner == player && is_gate(b.kind) && b.settings.gate != GateMode::Open) {
            for (x, y) in passage_tiles(b) {
                shut[idx(x as i32, y as i32)] = true;
            }
        }
        let mut goal = vec![false; (w * h) as usize];
        let mut any_goal = false;
        // Verbündete zählen nicht als Gegner: ihre Bergfriede und die Startplätze daneben fallen weg.
        let friendly: Vec<(f32, f32)> = self
            .buildings
            .iter()
            .filter(|b| b.kind == BuildingKind::Keep && (b.owner == player || self.allied(player, b.owner)))
            .map(|b| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0))
            .collect();
        for &(sx, sy) in &self.map.spawns {
            if friendly.iter().all(|c| (sx as f32 - c.0).hypot(sy as f32 - c.1) > OWN_SPAWN) {
                goal[idx(sx as i32, sy as i32)] = true;
                any_goal = true;
            }
        }
        // Fremde Bergfriede auch (der Startplatz selbst ist oft verbaut): die Felder davor.
        for b in self.buildings.iter().filter(|b| b.owner != player && b.kind == BuildingKind::Keep && !self.allied(player, b.owner)) {
            if let Some((ex, ey)) = b.entrance {
                goal[idx(ex as i32, ey as i32)] = true;
                any_goal = true;
            }
        }
        // Allein auf der Karte: der Kartenrand.
        let edge = !any_goal;

        let nav = &self.map.nav;
        let mut came = vec![u32::MAX; (w * h) as usize];
        let s = (start.0 as i32, start.1 as i32);
        came[idx(s.0, s.1)] = idx(s.0, s.1) as u32;
        let mut queue = VecDeque::from([s]);
        let mut found = None;
        while let Some((x, y)) = queue.pop_front() {
            if goal[idx(x, y)] || (edge && (x == 0 || y == 0 || x == w - 1 || y == h - 1)) {
                found = Some((x, y));
                break;
            }
            for (dx, dy) in DIRS {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h || came[idx(nx, ny)] != u32::MAX || shut[idx(nx, ny)] {
                    continue;
                }
                // Ziel ist oft ein Gebäudefeld (fremder Startplatz): betreten darf man es nicht,
                // aber daneben stehen reicht.
                if !nav.can_step(x, y, dx, dy) {
                    if goal[idx(nx, ny)] && dx.abs() + dy.abs() == 1 {
                        found = Some((nx, ny));
                        came[idx(nx, ny)] = idx(x, y) as u32;
                        break;
                    }
                    continue;
                }
                came[idx(nx, ny)] = idx(x, y) as u32;
                queue.push_back((nx, ny));
            }
            if found.is_some() {
                break;
            }
        }
        let Some(end) = found else { return CastleStatus { walled, closed: true, gap: None } };

        // Weg zurück zum Bergfried, dann vom Bergfried aus das erste Feld neben eigener Befestigung.
        let mut path = vec![end];
        let mut at = idx(end.0, end.1);
        while came[at] as usize != at {
            at = came[at] as usize;
            path.push(((at as i32) % w, (at as i32) / w));
        }
        path.reverse();
        let mut fort = vec![false; (w * h) as usize];
        for b in self.buildings.iter().filter(|b| b.owner == player && b.kind != BuildingKind::Keep) {
            if self.economy.building(b.kind).is_some_and(|d| d.category == BuildingCategory::Fortification) {
                for y in b.y..b.y + b.h {
                    for x in b.x..b.x + b.w {
                        fort[idx(x as i32, y as i32)] = true;
                    }
                }
            }
        }
        let near_fort = |&(x, y): &(i32, i32)| {
            (-1..=1).any(|dy| (-1..=1).any(|dx| {
                let (nx, ny) = (x + dx, y + dy);
                nx >= 0 && ny >= 0 && nx < w && ny < h && fort[idx(nx, ny)]
            }))
        };
        // Am Bergfried selbst steht oft eine Treppe oder ein Turm: erst ab ein paar Feldern Abstand.
        let gap = path
            .iter()
            .filter(|&&(x, y)| (x as f32 + 0.5 - center.0).hypot(y as f32 + 0.5 - center.1) > keep.w.max(keep.h) as f32)
            .find(|t| near_fort(t))
            .map(|&(x, y)| (x as u32, y as u32));
        CastleStatus { walled, closed: false, gap }
    }
}

impl World {
    /// Nur Tests: von jeder Ware viel ins Lager.
    #[cfg(test)]
    pub fn give_everything_for_tests(&mut self, player: u32) {
        for g in self.economy.def.goods.clone() {
            if let Some(s) = self.stocks.get_mut(&player) {
                s.add_unlimited(g, 1_000_000);
            }
        }
    }
}

/// Kurzform: Muss ein Gegner erst durch eine Mauer?
pub fn castle_closed(world: &World, player: u32) -> bool {
    world.castle_status(player).closed
}

#[cfg(test)]
mod tests {
    use crate::{economy::Economy, map::GameMap, world::World};
    use protocol::BuildingKind;

    #[test]
    fn ohne_mauer_offen_mit_ring_zu() {
        // Leere Karte, zwei Startplätze.
        let text = format!("spawn 15 20\nspawn 50 20\n{}", format!("{}\n", "0".repeat(60)).repeat(40));
        let map = GameMap::parse(&text).unwrap();
        let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_build_radius();
        let mut world = World::new(map, economy);
        world.spawn_player_at(1, (15, 20));
        let st = world.castle_status(1);
        assert!(!st.closed && !st.walled);
        world.give_everything_for_tests(1);
        // Ring aus Palisaden um den Bergfried, mit einer Lücke im Osten.
        let keep = world.buildings.iter().find(|b| b.kind == BuildingKind::Keep).unwrap().clone();
        let (x0, y0, x1, y1) = (keep.x as i32 - 6, keep.y as i32 - 6, (keep.x + keep.w) as i32 + 5, (keep.y + keep.h) as i32 + 5);
        let gap = (x1, (y0 + y1) / 2);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let ring = x == x0 || x == x1 || y == y0 || y == y1;
                if ring && (x, y) != gap {
                    world.place_building_rotated(1, BuildingKind::Palisade, x as u32, y as u32, 0, 1).unwrap();
                }
            }
        }
        let st = world.castle_status(1);
        assert!(st.walled && !st.closed, "{st:?}");
        let (gx, gy) = st.gap.unwrap();
        assert!((gx as i32 - gap.0).abs() <= 1 && (gy as i32 - gap.1).abs() <= 1, "Lücke {:?} statt {gap:?}", st.gap);
        world.place_building_rotated(1, BuildingKind::Palisade, gap.0 as u32, gap.1 as u32, 0, 1).unwrap();
        assert!(world.castle_status(1).closed);
    }
}
