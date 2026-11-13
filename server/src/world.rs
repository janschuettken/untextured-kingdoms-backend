//! Spielzustand: Einheiten auf der Karte. Wird nur im Spieltakt verändert.

use protocol::{UnitKind, UnitState};

use crate::map::GameMap;

/// Laufgeschwindigkeit des Königs in Feldern pro Sekunde.
const KING_SPEED: f32 = 3.0;

pub struct Unit {
    pub id: u32,
    pub owner: u32,
    pub kind: UnitKind,
    pub pos: (f32, f32),
    /// Noch abzulaufende Wegpunkte, der nächste zuerst.
    path: Vec<(f32, f32)>,
}

pub struct World {
    pub map: GameMap,
    pub units: Vec<Unit>,
    next_unit_id: u32,
}

impl World {
    pub fn new(map: GameMap) -> World {
        World { map, units: Vec::new(), next_unit_id: 1 }
    }

    /// Neuer Spieler: bekommt einen König auf einem freien Startfeld.
    pub fn spawn_player(&mut self, player: u32) {
        let taken: Vec<(u32, u32)> = self
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::King)
            .map(|u| (u.pos.0 as u32, u.pos.1 as u32))
            .collect();
        let spawns = &self.map.spawns;
        let (x, y) = spawns
            .iter()
            .copied()
            .find(|s| !taken.contains(s))
            .unwrap_or(spawns[player as usize % spawns.len()]);
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        self.units.push(Unit {
            id,
            owner: player,
            kind: UnitKind::King,
            pos: (x as f32 + 0.5, y as f32 + 0.5),
            path: Vec::new(),
        });
    }

    pub fn remove_player(&mut self, player: u32) {
        self.units.retain(|u| u.owner != player);
    }

    /// Bewegungsbefehl eines Spielers. Fremde Einheiten werden ignoriert.
    pub fn order_move(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32)) {
        if !target.0.is_finite() || !target.1.is_finite() {
            return;
        }
        let target = (
            target.0.clamp(0.0, self.map.width as f32 - 0.01),
            target.1.clamp(0.0, self.map.height as f32 - 0.01),
        );
        for unit in self.units.iter_mut() {
            if unit.owner == player && unit_ids.contains(&unit.id) {
                unit.path = self.map.nav.waypoints(unit.pos, target);
            }
        }
    }

    /// Einen Spieltakt von `dt` Sekunden simulieren.
    pub fn step(&mut self, dt: f32) {
        for unit in self.units.iter_mut() {
            let mut budget = KING_SPEED * dt;
            while budget > 0.0 {
                let Some(&next) = unit.path.first() else { break };
                let (dx, dy) = (next.0 - unit.pos.0, next.1 - unit.pos.1);
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= budget {
                    unit.pos = next;
                    unit.path.remove(0);
                    budget -= dist;
                } else {
                    unit.pos.0 += dx / dist * budget;
                    unit.pos.1 += dy / dist * budget;
                    budget = 0.0;
                }
            }
        }
    }

    pub fn snapshot(&self) -> Vec<UnitState> {
        self.units
            .iter()
            .map(|u| UnitState {
                id: u.id,
                owner: u.owner,
                kind: u.kind,
                x: u.pos.0,
                y: u.pos.1,
                moving: !u.path.is_empty(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::DEFAULT_MAP;

    #[test]
    fn koenig_laeuft_aus_der_burg_ins_tal() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap());
        world.spawn_player(1);
        world.spawn_player(2);
        let id = world.units[0].id;
        world.order_move(1, &[id], (30.5, 20.5));
        world.order_move(1, &[world.units[1].id], (0.5, 0.5)); // fremde Einheit: ignoriert
        assert!(world.units[1].path.is_empty());
        for _ in 0..20 * 60 {
            world.step(0.05);
            let u = &world.units[0];
            let (x, y) = (u.pos.0.floor() as i32, u.pos.1.floor() as i32);
            assert!(world.map.nav.is_open(x, y), "König steht auf ({x}, {y}) im Wasser/Fels");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 30.5).abs() < 1e-3 && (u.pos.1 - 20.5).abs() < 1e-3);
    }
}
