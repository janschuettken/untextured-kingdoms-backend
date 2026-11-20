//! Bewohner: Arbeiter und Träger brauchen eine Wohnung und Essen.
//!
//! Jeder Betrieb braucht `workers` Arbeiter, jeder Träger und Karren
//! `population.per_transport`. Platz gibt es nur in Wohnhäusern (`housing` je
//! Stufe). Wer keinen Platz hat, arbeitet nicht: zuerst bekommen Träger und
//! Karren eine Wohnung (sonst bewegt sich nichts), dann die Betriebe in der
//! Reihenfolge, in der sie gebaut wurden.
//!
//! Alle `meal_seconds` isst jeder arbeitende Bewohner eine Einheit Essen aus
//! dem Vorrat (von der Ware, von der am meisten da ist; Träger und Karren
//! `per_transport`). Jeder isst zu seiner eigenen Zeit, die Zeiten sind zufällig
//! über die Spanne verteilt. Wer nichts bekommt, hungert: nur er arbeitet und
//! läuft bis zu seiner nächsten Mahlzeit langsamer.

use std::collections::{BTreeMap, HashMap, HashSet};

use protocol::{GoodAmount, PeopleState, UnitKind};

use super::World;

impl World {
    pub(super) fn step_people(&mut self, dt: f32) {
        let pop = self.economy.def.population.clone();
        let players: Vec<u32> = self.stocks.keys().copied().collect();
        let mut unstaffed = HashSet::new();
        let mut idle = HashSet::new();
        let mut state_by_player: Vec<(u32, PeopleState)> = Vec::new();
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
                let need = self.economy.building(b.kind).map_or(0, |d| d.at(b.level).workers);
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

            state_by_player.push((player, PeopleState { housing, needed, working: used, ..Default::default() }));
        }
        self.idle_units = idle;
        let unstaffed_now = unstaffed.clone();

        // Essen: jeder arbeitende Bewohner (Arbeiter, Träger, Karren) isst alle
        // `meal_seconds` für sich, zu seiner eigenen Zeit (verteilt über die Minute).
        let meal = pop.meal_seconds;
        let tax = self.economy.def.market.tax_per_meal;
        let mut soonest: HashMap<u32, f32> = HashMap::new();
        let mut hungry_count: HashMap<u32, u32> = HashMap::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            let portion = match u.kind {
                UnitKind::Worker if !u.home.is_some_and(|h| unstaffed_now.contains(&h)) => 1,
                UnitKind::Carrier | UnitKind::Cart if !self.idle_units.contains(&u.id) => pop.per_transport,
                _ => {
                    if self.units[k].hungry || self.units[k].meal_in.is_some() {
                        self.units[k].hungry = false;
                        self.units[k].meal_in = None;
                    }
                    continue;
                }
            };
            if meal <= 0.0 || portion == 0 {
                self.units[k].hungry = false;
                continue;
            }
            let (owner, id) = (u.owner, u.id);
            // Die erste Mahlzeit kommt zu einer zufälligen Zeit, danach regelmäßig.
            let mut left = u.meal_in.unwrap_or_else(|| spread(id) * meal) - dt;
            if left <= 0.0 {
                left += meal;
                let Some(stock) = self.stocks.get_mut(&owner) else { continue };
                let mut eaten = 0;
                while eaten < portion {
                    let Some(&good) = pop.food.iter().filter(|g| stock.amount(**g) > 0).max_by_key(|g| stock.amount(**g)) else { break };
                    let take = stock.amount(good).min(portion - eaten);
                    stock.take_all(&[GoodAmount { good, amount: take }]);
                    eaten += take;
                }
                // Satte Bewohner zahlen Steuern.
                stock.taler = stock.taler.saturating_add(eaten * tax);
                self.units[k].hungry = eaten < portion;
                self.dirty_stocks.insert(owner);
            }
            self.units[k].meal_in = Some(left.max(0.0));
            if self.units[k].hungry {
                *hungry_count.entry(owner).or_default() += portion;
            }
            let s = soonest.entry(owner).or_insert(f32::MAX);
            *s = s.min(left);
        }
        // Je Betrieb: wie stark hungernde Arbeiter ihn bremsen.
        let slow = self.hungry_speed();
        let mut per: HashMap<u32, (u32, u32)> = HashMap::new();
        for u in self.units.iter().filter(|u| u.kind == UnitKind::Worker) {
            let Some(h) = u.home else { continue };
            let e = per.entry(h).or_default();
            e.0 += 1;
            e.1 += u.hungry as u32;
        }
        self.worker_hunger = per.into_iter().filter(|(_, (_, h))| *h > 0).map(|(b, (n, h))| (b, ((n - h) as f32 + h as f32 * slow) / n as f32)).collect();
        for (player, mut state) in state_by_player {
            let hungry = hungry_count.get(&player).copied().unwrap_or(0);
            state.hungry = hungry > 0;
            state.hungry_count = hungry;
            state.fed = state.working.saturating_sub(hungry);
            self.meal_soonest.insert(player, soonest.get(&player).copied().filter(|s| *s < f32::MAX).unwrap_or(0.0));
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
            for s in def.levels.iter().take(b.level.max(1) as usize).flat_map(|l| &l.spawns) {
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

    /// Tempo-Faktor einer hungrigen Einheit (Arbeit und Laufen).
    pub(super) fn hungry_speed(&self) -> f32 {
        self.economy.def.population.hungry_speed_percent as f32 / 100.0
    }

    /// Tempo-Faktor einer Einheit: nur wer selbst hungert, ist langsamer.
    pub(super) fn unit_hunger_factor(&self, k: usize) -> f32 {
        if self.units[k].hungry { self.hungry_speed() } else { 1.0 }
    }

    /// Tempo-Faktor eines Betriebs: Anteil seiner Arbeiter, die hungern, bremst ihn.
    pub(super) fn building_hunger_factor(&self, building: u32) -> f32 {
        self.worker_hunger.get(&building).copied().unwrap_or(1.0)
    }
}

/// Fester Zufallswert 0..1 je Einheit: verteilt die Essenszeiten über die Minute.
fn spread(id: u32) -> f32 {
    let mut x = (id as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
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
        w.test_granary(1); // sonst verdirbt das Essen
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
        assert_eq!(w.stock_state(1).people.hungry_count, 2);
        let k = w.units.iter().position(|u| u.kind == protocol::UnitKind::Worker).unwrap();
        assert_eq!(w.unit_hunger_factor(k), 0.5);
    }

    #[test]
    fn jeder_isst_fuer_sich() {
        let mut w = world();
        w.economy.def.population.meal_seconds = 60.0;
        w.admin_give(1, &[protocol::GoodAmount { good: Good::Wood, amount: 200 }, protocol::GoodAmount { good: Good::Boards, amount: 200 }], 0);
        w.place_building(1, BuildingKind::House, 20, 2).unwrap();
        w.place_building(1, BuildingKind::House, 20, 8).unwrap();
        for (x, y) in [(2, 2), (2, 8), (2, 14), (8, 2), (8, 8), (8, 14)] {
            w.place_building(1, BuildingKind::Woodcutter, x, y).unwrap();
        }
        // Essenszeiten sind über die Minute verteilt, nicht alle gleichzeitig.
        w.step(0.05);
        let times: Vec<f32> = w.units.iter().filter_map(|u| u.meal_in).collect();
        assert!(times.len() >= 6);
        let (lo, hi) = times.iter().fold((f32::MAX, 0.0f32), |(a, b), &t| (a.min(t), b.max(t)));
        assert!(hi - lo > 10.0, "verteilt: {times:?}");
        // Kein Essen: wer schon dran war, hungert, die anderen noch nicht.
        let s = w.stock_state(1);
        for g in s.goods.iter().filter(|g| w.economy.def.population.food.contains(&g.good)) {
            w.stocks.get_mut(&1).unwrap().take_all(&[*g]);
        }
        let mid = (lo + hi) / 2.0;
        for _ in 0..(mid / 0.05) as usize {
            w.step(0.05);
        }
        let hungry = w.units.iter().filter(|u| u.hungry).count();
        assert!(hungry > 0 && hungry < times.len(), "nur einige hungern: {hungry}");
        assert_eq!(w.stock_state(1).people.hungry_count as usize, hungry);
        let k = w.units.iter().position(|u| u.meal_in.is_some() && !u.hungry).unwrap();
        assert_eq!(w.unit_hunger_factor(k), 1.0, "satte sind nicht langsamer");
    }
}
