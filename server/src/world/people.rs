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

use std::collections::HashSet;

use protocol::{GoodAmount, PeopleState};

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

            let mut state = PeopleState { housing, needed, working: used, hungry: self.people.get(&player).is_some_and(|p| p.hungry) };
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
            }
            if self.people.get(&player) != Some(&state) {
                self.people.insert(player, state);
                self.dirty_stocks.insert(player);
            }
        }
        self.unstaffed = unstaffed;
        self.idle_units = idle;
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
        let mut eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
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
        assert_eq!((w.people[&1].housing, w.people[&1].working), (4, 1));
    }

    #[test]
    fn traeger_warten_auf_wohnung() {
        let mut w = world();
        w.admin_give(1, &[protocol::GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap();
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
