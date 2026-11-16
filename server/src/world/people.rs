//! Bewohner: Arbeiter und Träger brauchen eine Wohnung und Essen.
//!
//! Jeder Betrieb braucht `workers` Arbeiter, jeder Träger und Karren
//! `population.per_transport`. Platz gibt es nur in Wohnhäusern (`housing` je
//! Stufe). Wer keinen Platz hat, arbeitet nicht: zuerst bekommen Träger und
//! Karren eine Wohnung (sonst bewegt sich nichts), dann die Betriebe in der
//! Reihenfolge, in der sie gebaut wurden.
//!
//! Alle `meal_seconds` isst jeder arbeitende Bewohner eine Einheit Essen aus
//! dem Vorrat (von der Ware, von der am meisten da ist). Reicht es nicht, sind
//! alle hungrig und arbeiten und laufen bis zur nächsten Mahlzeit langsamer.

use std::collections::{BTreeMap, HashSet};

use protocol::{GoodAmount, PeopleState, UnitKind};

use super::World;

impl World {
    pub(super) fn step_people(&mut self, dt: f32) {
        let pop = self.economy.def.population.clone();
        let players: Vec<u32> = self.stocks.keys().copied().collect();
        let mut unstaffed = HashSet::new();
        let mut idle = HashSet::new();
        for player in players {
            let housing: u32 = self
                .buildings
                .iter()
                .filter(|b| b.owner == player)
                .filter_map(|b| self.economy.building(b.kind)?.levels.get(b.level.max(1) as usize - 1))
                .map(|l| l.housing)
                .sum();
            let mut used = 0;
            let mut needed = 0;
            for u in self.units.iter().filter(|u| u.owner == player && u.is_transport()) {
                needed += pop.per_transport;
                if used + pop.per_transport <= housing {
                    used += pop.per_transport;
                } else {
                    idle.insert(u.id);
                }
            }
            for b in self.buildings.iter().filter(|b| b.owner == player) {
                let need = self.economy.building(b.kind).map_or(0, |d| d.workers);
                if need == 0 {
                    continue;
                }
                needed += need;
                if used + need <= housing {
                    used += need;
                } else {
                    unstaffed.insert(b.id);
                }
            }

            let prev = self.people.get(&player);
            let mut state = PeopleState {
                housing,
                needed,
                working: used,
                hungry: prev.is_some_and(|p| p.hungry),
                fed: prev.map_or(0, |p| p.fed),
                next_meal_in: 0,
            };
            let timer = self.meal_timer.entry(player).or_insert(0.0);
            *timer += dt;
            if pop.meal_seconds > 0.0 && *timer >= pop.meal_seconds {
                *timer -= pop.meal_seconds;
                let stock = self.stocks.get_mut(&player).expect("Spieler hat einen Vorrat");
                let mut eaten = 0;
                while eaten < used {
                    let Some(&good) = pop.food.iter().filter(|g| stock.amount(**g) > 0).max_by_key(|g| stock.amount(**g)) else { break };
                    let take = stock.amount(good).min(used - eaten);
                    stock.take_all(&[GoodAmount { good, amount: take }]);
                    eaten += take;
                }
                // Satte Bewohner zahlen Steuern.
                stock.taler = stock.taler.saturating_add(eaten * self.economy.def.market.tax_per_meal);
                if eaten > 0 {
                    self.dirty_stocks.insert(player);
                }
                state.hungry = eaten < used;
                state.fed = eaten.min(used);
                self.dirty_stocks.insert(player); // neue Zeit bis zur nächsten Mahlzeit
            }
            if self.people.get(&player) != Some(&state) {
                self.people.insert(player, state);
                self.dirty_stocks.insert(player);
            }
        }
        // Wer gerade eine Wohnung bekommen hat, kommt aus dem Wohnhaus zur Arbeit.
        let housed: Vec<u32> = self.unstaffed.difference(&unstaffed).copied().collect();
        self.unstaffed = unstaffed;
        for id in housed {
            self.send_workers_from_home(id);
        }
        self.idle_units = idle;
        self.step_respawn(dt);
    }

    /// Gefallene Arbeiter, Träger und Karren werden nach `army.civilian_respawn_seconds`
    /// ersetzt, einer nach dem anderen (Arbeiter kommen aus dem Wohnhaus).
    fn step_respawn(&mut self, dt: f32) {
        let secs = self.economy.def.army.civilian_respawn_seconds;
        if secs <= 0.0 {
            self.respawn_clock.clear();
            return;
        }
        let mut have: BTreeMap<(u32, UnitKind), u32> = BTreeMap::new();
        for u in &self.units {
            if let Some(h) = u.home {
                *have.entry((h, u.kind)).or_default() += 1;
            }
        }
        let mut due: Vec<(usize, UnitKind)> = Vec::new();
        let mut missing_any: HashSet<u32> = HashSet::new();
        for (bi, b) in self.buildings.iter().enumerate() {
            let Some(def) = self.economy.building(b.kind) else { continue };
            let mut totals: BTreeMap<UnitKind, u32> = BTreeMap::new();
            totals.insert(UnitKind::Worker, self.worker_slots(b));
            for s in def.spawns.iter().chain(def.levels.iter().take(b.level.max(1) as usize).flat_map(|l| &l.spawns)) {
                *totals.entry(s.kind).or_default() += s.count;
            }
            let Some((&kind, _)) = totals.iter().find(|(k, n)| have.get(&(b.id, **k)).copied().unwrap_or(0) < **n) else { continue };
            missing_any.insert(b.id);
            let clock = self.respawn_clock.entry(b.id).or_insert(0.0);
            *clock += dt;
            if *clock >= secs {
                *clock = 0.0;
                due.push((bi, kind));
            }
        }
        self.respawn_clock.retain(|id, _| missing_any.contains(id));
        for (bi, kind) in due {
            if kind == UnitKind::Worker {
                self.spawn_worker_units(bi, 1);
            } else {
                let b = self.buildings[bi].clone();
                self.spawn_transport(&b, kind, 0);
            }
        }
    }

    /// Tempo-Faktor für Arbeit und Laufen: hungrig langsamer.
    pub(super) fn hunger_factor(&self, player: u32) -> f32 {
        if self.people.get(&player).is_some_and(|p| p.hungry) {
            self.economy.def.population.hungry_speed_percent as f32 / 100.0
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, BuildingStatus, Good};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn world() -> World {
        let row = "0".repeat(30);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(30));
        let mut eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building();
        eco.def.population.meal_seconds = 10.0;
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w
    }

    #[test]
    fn ohne_haus_kein_arbeiter() {
        let mut w = world();
        let saw = w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        w.step(0.05);
        let b = w.buildings.iter().find(|b| b.id == saw.id).unwrap();
        assert_eq!(b.status, BuildingStatus::NoWorkers);
        assert_eq!(w.people[&1].needed, 1);
        assert_eq!(w.people[&1].working, 0);

        w.place_building(1, BuildingKind::House, 10, 2).unwrap();
        w.step(0.05);
        let b = w.buildings.iter().find(|b| b.id == saw.id).unwrap();
        assert_ne!(b.status, BuildingStatus::NoWorkers);
        let housing = w.economy.building(BuildingKind::House).unwrap().levels[0].housing;
        assert_eq!((w.people[&1].housing, w.people[&1].working), (housing, 1));
    }

    #[test]
    fn arbeiter_kommen_aus_dem_wohnhaus() {
        let mut w = world();
        let door = |w: &World, id: u32| {
            let b = w.buildings.iter().find(|b| b.id == id).unwrap();
            let (ex, ey) = b.entrance.unwrap();
            (ex as f32 + 0.5, ey as f32 + 0.5)
        };
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1) < 0.6;
        // Erst der Betrieb, dann das Haus: der Arbeiter erscheint am Haus, nicht am Betrieb.
        let saw = w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        w.step(0.05);
        let house = w.place_building(1, BuildingKind::House, 20, 20).unwrap();
        w.step(0.05);
        let worker = w.units.iter().find(|u| u.home == Some(saw.id)).unwrap();
        assert!(near(worker.pos, door(&w, house.id)) || !worker.path.is_empty() && worker.pos.1 > 15.0, "am Haus: {:?}", worker.pos);
        assert!(!worker.path.is_empty(), "läuft zur Arbeit");
        // Mit Haus: neue Arbeiter starten gleich dort.
        let wood = w.place_building(1, BuildingKind::Woodcutter, 2, 10).unwrap();
        let worker = w.units.iter().find(|u| u.home == Some(wood.id)).unwrap();
        assert!(near(worker.pos, door(&w, house.id)), "am Haus: {:?}", worker.pos);
    }

    #[test]
    fn gefallene_zivilisten_werden_ersetzt() {
        let mut w = world();
        w.place_building(1, BuildingKind::House, 20, 20).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        w.units.retain(|u| u.home != Some(saw.id));
        let secs = w.economy.def.army.civilian_respawn_seconds;
        for _ in 0..((secs - 1.0) * 20.0) as u32 {
            w.step(0.05);
        }
        assert!(!w.units.iter().any(|u| u.home == Some(saw.id)), "noch kein Ersatz");
        for _ in 0..40 {
            w.step(0.05);
        }
        assert_eq!(w.units.iter().filter(|u| u.home == Some(saw.id) && u.kind == protocol::UnitKind::Worker).count(), 1);
    }

    #[test]
    fn traeger_warten_auf_wohnung() {
        let mut w = world();
        w.admin_give(1, &[protocol::GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::CartShed, 2, 2).unwrap();
        w.step(0.05);
        assert_eq!(w.idle_units.len(), 3);
        w.place_building(1, BuildingKind::House, 10, 2).unwrap();
        w.step(0.05);
        assert!(w.idle_units.is_empty());
        assert_eq!(w.people[&1].working, 3);
    }

    #[test]
    fn essen_und_hunger() {
        let mut w = world();
        w.place_building(1, BuildingKind::House, 10, 2).unwrap();
        w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        w.place_building(1, BuildingKind::Woodcutter, 2, 10).unwrap();
        let bread = |w: &World| w.stock_state(1).goods.iter().find(|g| g.good == Good::Bread).map_or(0, |g| g.amount);
        let before = bread(&w);
        let taler = w.stock_state(1).taler;
        for _ in 0..(10.0 / 0.05) as usize + 2 {
            w.step(0.05);
        }
        assert_eq!(before - bread(&w), 2, "zwei Arbeiter essen je ein Brot");
        assert!(!w.people[&1].hungry);
        assert_eq!(w.stock_state(1).taler, taler + 2 * w.economy.def.market.tax_per_meal, "satte Bewohner zahlen Steuern");
        // Alles Essen weg: hungrig.
        let s = w.stock_state(1);
        for g in s.goods.iter().filter(|g| w.economy.def.population.food.contains(&g.good)) {
            w.stocks.get_mut(&1).unwrap().take_all(&[*g]);
        }
        for _ in 0..(10.0 / 0.05) as usize + 2 {
            w.step(0.05);
        }
        assert!(w.people[&1].hungry);
        assert!(w.stock_state(1).people.hungry);
        assert_eq!(w.hunger_factor(1), 0.5);
    }
}
