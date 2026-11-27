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
    /// Alt: gemeinsame Essenszeit je Spieler (heute isst jeder Bewohner für sich).
    #[serde(default)]
    meal_timer: HashMap<u32, f32>,
    #[serde(default)]
    hub_goods: HashMap<u32, super::storage::HubGoods>,
    next_unit_id: u32,
    next_building_id: u32,
    #[serde(default)]
    trade_mode: protocol::TradeMode,
    #[serde(default)]
    shot_mode: protocol::ShotMode,
    #[serde(default)]
    rules: protocol::GameRules,
    #[serde(default)]
    eliminated: std::collections::BTreeSet<u32>,
    #[serde(default)]
    teams: HashMap<u32, u32>,
    #[serde(default)]
    winners: Option<Vec<u32>>,
    #[serde(default)]
    auto_trade: HashMap<u32, Vec<protocol::AutoTrade>>,
    #[serde(default)]
    good_limits: HashMap<u32, Vec<protocol::GoodLimit>>,
    #[serde(default)]
    policies: HashMap<u32, (u32, u32)>,
    #[serde(default)]
    game_seconds: f64,
    /// Preisdynamik am Markt: Faktor je Ware.
    #[serde(default)]
    price_factor: HashMap<protocol::Good, f32>,
    #[serde(default)]
    timeline: Vec<protocol::TimelineEntry>,
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
            meal_timer: HashMap::new(),
            hub_goods: self.hub_goods.clone(),
            next_unit_id: self.next_unit_id,
            next_building_id: self.next_building_id,
            trade_mode: self.trade_mode,
            shot_mode: self.shot_mode,
            rules: self.rules,
            eliminated: self.eliminated.clone(),
            teams: self.teams.clone(),
            winners: self.winners.clone(),
            auto_trade: self.auto_trade.clone(),
            good_limits: self.good_limits.clone(),
            policies: self.policies.clone(),
            price_factor: self.price_factor.clone(),
            game_seconds: self.game_seconds,
            timeline: self.timeline.clone(),
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
        // Die Brücken der Karte stehen im Spielstand (vielleicht schon eingerissen).
        for b in std::mem::take(&mut w.buildings) {
            w.set_footprint(&b, false);
        }
        for (t, &r) in save.resources.iter().enumerate() {
            w.set_resource(t, r);
        }
        w.resource_left = save.resource_left;
        for b in &save.buildings {
            w.set_footprint(b, true);
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
        w.hub_goods = save.hub_goods;
        w.next_unit_id = save.next_unit_id;
        w.next_building_id = save.next_building_id;
        w.trade_mode = save.trade_mode;
        w.shot_mode = save.shot_mode;
        w.rules = save.rules;
        w.eliminated = save.eliminated;
        w.teams = save.teams;
        w.winners = save.winners;
        w.auto_trade = save.auto_trade;
        w.good_limits = save.good_limits;
        w.policies = save.policies;
        w.price_factor = save.price_factor;
        w.game_seconds = save.game_seconds;
        w.timeline = save.timeline;
        // Ältere Stände: Herstellungsbetriebe hatten noch keine eigenen Arbeiter.
        for bi in 0..w.buildings.len() {
            let id = w.buildings[bi].id;
            if !w.units.iter().any(|u| u.home == Some(id) && u.kind == protocol::UnitKind::Worker) {
                w.spawn_workers(bi);
            }
        }
        w.update_inventory_capacities();
        // Lagerplatz neu berechnen: die Lagerarten können sich seitdem geändert haben.
        let players: Vec<u32> = w.stocks.keys().copied().collect();
        for p in players {
            w.update_capacity(p);
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

    /// Spielt mit oder ist schon ausgeschieden (dann kommt er nicht neu ins Spiel).
    pub fn has_player(&self, player: u32) -> bool {
        self.stocks.contains_key(&player) || self.eliminated.contains(&player)
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good};

    use crate::{economy::Economy, map::GameMap, world::World};

    #[test]
    fn speichern_und_laden() {
        let text = format!("spawn 1 1\n[terrain]\n{}[resources]\n{}", format!("{}\n", "0".repeat(20)).repeat(10), format!("{}WW\n", ".".repeat(18)).repeat(10));
        let eco = || Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
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
