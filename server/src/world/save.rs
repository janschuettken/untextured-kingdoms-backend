//! Spielstand speichern und laden.
//!
//! Gespeichert wird alles, was sich im Spiel ändert: Einheiten, Gebäude,
//! Vorräte, Rohstoffe auf der Karte. Karte und Wirtschaftsdaten selbst nicht:
//! Beim Laden muss dieselbe Karte laufen; es gelten die aktuellen
//! Wirtschaftsdaten.

use std::collections::HashMap;

use protocol::{BuildingState, PeopleState, ResourceKind};
use serde::{Deserialize, Serialize};

use super::{production::Work, Unit, World};
use crate::{economy::Economy, map::GameMap, stock::Stock};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct SaveGame {
    pub version: u32,
    /// Name der Karte, auf der gespielt wurde.
    pub map: String,
    width: u32,
    height: u32,
    resources: Vec<ResourceKind>,
    resource_left: Vec<u32>,
    units: Vec<Unit>,
    buildings: Vec<BuildingState>,
    stocks: HashMap<u32, Stock>,
    inventories: HashMap<u32, Stock>,
    work: HashMap<u32, Work>,
    people: HashMap<u32, PeopleState>,
    meal_timer: HashMap<u32, f32>,
    next_unit_id: u32,
    next_building_id: u32,
}

impl World {
    pub fn to_save(&self, map_name: &str) -> SaveGame {
        SaveGame {
            version: VERSION,
            map: map_name.to_string(),
            width: self.map.width,
            height: self.map.height,
            resources: self.map.resources.clone(),
            resource_left: self.resource_left.clone(),
            units: self.units.clone(),
            buildings: self.buildings.clone(),
            stocks: self.stocks.clone(),
            inventories: self.inventories.clone(),
            work: self.work.clone(),
            people: self.people.clone(),
            meal_timer: self.meal_timer.clone(),
            next_unit_id: self.next_unit_id,
            next_building_id: self.next_building_id,
        }
    }

    /// Spielstand auf eine frisch geladene Karte setzen.
    pub fn from_save(map: GameMap, economy: Economy, save: SaveGame) -> Result<World, String> {
        if save.version != VERSION {
            return Err(format!("Spielstand hat Version {}, erwartet {VERSION}", save.version));
        }
        if (save.width, save.height) != (map.width, map.height) || save.resources.len() != map.resources.len() {
            return Err("Spielstand passt nicht zur Karte".into());
        }
        let mut w = World::new(map, economy);
        for (t, &r) in save.resources.iter().enumerate() {
            w.set_resource(t, r);
        }
        w.resource_left = save.resource_left;
        for b in &save.buildings {
            for ty in b.y..b.y + b.h {
                for tx in b.x..b.x + b.w {
                    w.occupied[(ty * w.map.width + tx) as usize] = b.id;
                    if super::gate::blocks(b, tx, ty) {
                        w.map.nav.set_blocked(tx as i32, ty as i32, true);
                    }
                }
            }
            if let Some((ex, ey)) = b.entrance {
                w.entrances.insert((ey * w.map.width + ex) as usize, b.id);
            }
        }
        w.buildings = save.buildings;
        w.units = save.units;
        w.stocks = save.stocks;
        w.inventories = save.inventories;
        w.work = save.work;
        w.people = save.people;
        w.meal_timer = save.meal_timer;
        w.next_unit_id = save.next_unit_id;
        w.next_building_id = save.next_building_id;
        // Ältere Stände: Herstellungsbetriebe hatten noch keine eigenen Arbeiter.
        for bi in 0..w.buildings.len() {
            let id = w.buildings[bi].id;
            if !w.units.iter().any(|u| u.home == Some(id) && u.kind == protocol::UnitKind::Worker) {
                w.spawn_workers(bi);
            }
        }
        w.events.clear();
        Ok(w)
    }

    /// Alle Spieler, die es gibt (auch die gerade nicht verbunden sind).
    pub fn players(&self) -> Vec<u32> {
        let mut p: Vec<u32> = self.stocks.keys().copied().collect();
        p.sort();
        p
    }

    pub fn has_player(&self, player: u32) -> bool {
        self.stocks.contains_key(&player)
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good};

    use crate::{economy::Economy, map::GameMap, world::World};

    #[test]
    fn speichern_und_laden() {
        let text = format!("spawn 1 1\n[terrain]\n{}[resources]\n{}", format!("{}\n", "0".repeat(20)).repeat(10), format!("{}WW\n", ".".repeat(18)).repeat(10));
        let eco = || Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco());
        w.spawn_player(1);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let wc = w.place_building(1, BuildingKind::Woodcutter, 12, 2).unwrap();
        for _ in 0..600 {
            w.step(0.05);
        }
        w.set_resource(18, protocol::ResourceKind::None);
        let json = serde_json::to_string(&w.to_save("test")).unwrap();

        let save = serde_json::from_str(&json).unwrap();
        let mut l = World::from_save(GameMap::parse(&text).unwrap(), eco(), save).unwrap();
        assert_eq!(l.buildings.len(), w.buildings.len());
        assert_eq!(l.units.len(), w.units.len());
        assert_eq!(l.stocks[&1].amount(Good::Wood), w.stocks[&1].amount(Good::Wood));
        assert!(l.map.nav.is_open(18, 0), "abgeholzt bleibt abgeholzt");
        assert!(!l.map.nav.is_open(12, 2), "Gebäude blockiert");
        assert_eq!(l.place_building(1, BuildingKind::Wall, 13, 3).unwrap_err().key, "error.occupied");
        assert!(l.has_player(1));
        // Weiterspielen geht.
        for _ in 0..200 {
            l.step(0.05);
        }
        assert!(l.buildings.iter().any(|b| b.id == wc.id));
    }
}
