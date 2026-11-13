//! Spielzustand: Einheiten und Gebäude auf der Karte.

use protocol::{BuildingKind, BuildingState, TileKind, UnitKind, UnitState};

use crate::{buildings, map::GameMap};

/// Laufgeschwindigkeit des Königs in Feldern pro Sekunde.
const KING_SPEED: f32 = 3.0;

pub struct Unit {
    pub id: u32,
    pub owner: u32,
    pub kind: UnitKind,
    pub pos: (f32, f32),
    /// Noch abzulaufende Wegpunkte, der nächste zuerst.
    path: Vec<(f32, f32)>,
    /// Wohin die Einheit zuletzt geschickt wurde (für neue Wegsuche, wenn
    /// plötzlich ein Gebäude im Weg steht).
    target: Option<(f32, f32)>,
}

pub struct World {
    pub map: GameMap,
    pub units: Vec<Unit>,
    pub buildings: Vec<BuildingState>,
    /// Pro Feld die Id des Gebäudes darauf, 0 = frei.
    occupied: Vec<u32>,
    next_unit_id: u32,
    next_building_id: u32,
}

impl World {
    pub fn new(map: GameMap) -> World {
        let n = (map.width * map.height) as usize;
        World {
            map,
            units: Vec::new(),
            buildings: Vec::new(),
            occupied: vec![0; n],
            next_unit_id: 1,
            next_building_id: 1,
        }
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
            target: None,
        });
    }

    /// Spieler geht: seine Einheiten und Gebäude verschwinden.
    /// Liefert die Ids der entfernten Gebäude.
    pub fn remove_player(&mut self, player: u32) -> Vec<u32> {
        self.units.retain(|u| u.owner != player);
        let ids: Vec<u32> = self.buildings.iter().filter(|b| b.owner == player).map(|b| b.id).collect();
        for &id in &ids {
            self.remove_building(id);
        }
        ids
    }

    /// Gebäude errichten, wenn der Platz passt. Ein Gebäude braucht flachen
    /// Boden auf einer einzigen Höhenstufe, keine Rampe, kein Wasser, und darf
    /// weder ein anderes Gebäude noch eine Einheit überdecken.
    pub fn place_building(&mut self, player: u32, kind: BuildingKind, x: u32, y: u32) -> Result<BuildingState, String> {
        let spec = buildings::spec(kind);
        if let Some(max) = buildings::limit_per_player(kind) {
            let have = self.buildings.iter().filter(|b| b.owner == player && b.kind == kind).count();
            if have >= max {
                return Err(format!("Davon darfst du nur {max} bauen"));
            }
        }
        if x.checked_add(spec.w).is_none_or(|r| r > self.map.width)
            || y.checked_add(spec.h).is_none_or(|b| b > self.map.height)
        {
            return Err("Außerhalb der Karte".into());
        }
        let level = self.map.levels[(y * self.map.width + x) as usize];
        for ty in y..y + spec.h {
            for tx in x..x + spec.w {
                let i = (ty * self.map.width + tx) as usize;
                match self.map.tiles[i] {
                    TileKind::Water => return Err("Nicht auf Wasser".into()),
                    TileKind::Ramp => return Err("Nicht auf Rampen".into()),
                    TileKind::Ground => {}
                }
                if self.map.levels[i] != level {
                    return Err("Der Boden muss eben sein".into());
                }
                if self.occupied[i] != 0 {
                    return Err("Dort steht schon ein Gebäude".into());
                }
            }
        }
        let covers = |p: (f32, f32)| {
            let (ux, uy) = (p.0.floor() as i64, p.1.floor() as i64);
            ux >= x as i64 && uy >= y as i64 && ux < (x + spec.w) as i64 && uy < (y + spec.h) as i64
        };
        if self.units.iter().any(|u| covers(u.pos)) {
            return Err("Dort steht eine Einheit".into());
        }

        let b = BuildingState {
            id: self.next_building_id,
            owner: player,
            kind,
            x,
            y,
            w: spec.w,
            h: spec.h,
            hp: spec.max_hp,
        };
        self.next_building_id += 1;
        for ty in y..y + spec.h {
            for tx in x..x + spec.w {
                self.occupied[(ty * self.map.width + tx) as usize] = b.id;
                self.map.nav.set_blocked(tx as i32, ty as i32, true);
            }
        }
        self.buildings.push(b.clone());
        self.replan_all();
        Ok(b)
    }

    /// Eigenes Gebäude abreißen.
    pub fn demolish(&mut self, player: u32, id: u32) -> Result<(), String> {
        match self.buildings.iter().find(|b| b.id == id) {
            Some(b) if b.owner == player => {
                self.remove_building(id);
                self.replan_all();
                Ok(())
            }
            Some(_) => Err("Das ist nicht dein Gebäude".into()),
            None => Err("Gebäude gibt es nicht".into()),
        }
    }

    fn remove_building(&mut self, id: u32) {
        let Some(pos) = self.buildings.iter().position(|b| b.id == id) else { return };
        let b = self.buildings.remove(pos);
        for ty in b.y..b.y + b.h {
            for tx in b.x..b.x + b.w {
                self.occupied[(ty * self.map.width + tx) as usize] = 0;
                self.map.nav.set_blocked(tx as i32, ty as i32, false);
            }
        }
    }

    /// Nach einer Änderung am Gitter: laufende Einheiten suchen ihren Weg neu.
    fn replan_all(&mut self) {
        for unit in self.units.iter_mut() {
            if let (Some(target), false) = (unit.target, unit.path.is_empty()) {
                unit.path = self.map.nav.waypoints(unit.pos, target);
            }
        }
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
                unit.target = Some(target);
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

    fn flat_world() -> World {
        let row = "0".repeat(12);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(12));
        let mut world = World::new(GameMap::parse(&text).unwrap());
        world.spawn_player(1);
        world
    }

    #[test]
    fn bauregeln_werden_geprueft() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap());
        world.spawn_player(1);
        let water = (0..64u32 * 64).find(|&i| world.map.tiles[i as usize] == TileKind::Water).unwrap();
        assert!(world.place_building(1, BuildingKind::Wall, water % 64, water / 64).is_err());
        let ramp = (0..64u32 * 64).find(|&i| world.map.tiles[i as usize] == TileKind::Ramp).unwrap();
        assert!(world.place_building(1, BuildingKind::Wall, ramp % 64, ramp / 64).is_err());
        assert!(world.place_building(1, BuildingKind::Keep, 62, 62).is_err(), "ragt aus der Karte");
        let king = world.units[0].pos;
        assert!(world.place_building(1, BuildingKind::Wall, king.0 as u32, king.1 as u32).is_err());

        let mut flat = flat_world();
        let b = flat.place_building(1, BuildingKind::Tower, 4, 4).unwrap();
        assert_eq!((b.w, b.h), (2, 2));
        assert!(flat.place_building(1, BuildingKind::Wall, 5, 5).is_err(), "überlappt");
        assert!(!flat.map.nav.is_open(5, 5));
        flat.place_building(1, BuildingKind::Keep, 7, 7).unwrap();
        assert!(flat.place_building(1, BuildingKind::Keep, 0, 7).is_err(), "nur ein Bergfried");
        assert!(flat.demolish(2, b.id).is_err(), "fremdes Gebäude");
        flat.demolish(1, b.id).unwrap();
        assert!(flat.map.nav.is_open(5, 5));
    }

    #[test]
    fn mauer_im_weg_wird_umgangen() {
        let mut world = flat_world();
        let id = world.units[0].id;
        world.order_move(1, &[id], (10.5, 1.5));
        world.step(0.05);
        // Quer über den Weg eine Mauer mit Lücke ganz unten.
        for y in 0..11 {
            world.place_building(1, BuildingKind::Wall, 5, y).unwrap();
        }
        for _ in 0..20 * 30 {
            world.step(0.05);
            let u = &world.units[0];
            assert!(world.map.nav.is_open(u.pos.0 as i32, u.pos.1 as i32), "König läuft durch die Mauer");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 10.5).abs() < 1e-3 && (u.pos.1 - 1.5).abs() < 1e-3);
    }
}
