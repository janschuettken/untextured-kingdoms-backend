//! Bestand je Lager.
//!
//! Der Vorrat eines Spielers bleibt eine Summe (`stocks`), mit ihr wird gebaut,
//! gegessen und gehandelt. Zusätzlich merkt sich der Server, in welchem Lager
//! (Bergfried, Vorratslager) wie viel liegt: Träger und Arbeiter legen dort ab
//! und holen dort. Alles ohne festen Ort (Bauen, Essen, Markt, Admin) gleicht
//! `balance_hubs` nach jedem Takt aus: Neues kommt ins erste passende Lager,
//! Verbrauchtes geht vom größten Bestand ab.

use std::collections::{BTreeMap, BTreeSet};

use protocol::{BuildingKind, Good, GoodAmount};

use super::World;

/// Was ein neues Belagerungslager anfangs bereithalten will (Ware, Menge).
pub(super) const CAMP_DEFAULT: [(Good, u32); 4] = [(Good::Stone, 50), (Good::Boards, 40), (Good::Wood, 30), (Good::Iron, 10)];

/// Waren in einem Lager.
pub type HubGoods = BTreeMap<Good, u32>;

impl World {
    /// Ware in einem bestimmten Lager abgelegt (der Vorrat ist schon erhöht).
    pub(super) fn hub_add(&mut self, hub: u32, good: Good, n: u32) {
        if n > 0 {
            *self.hub_goods.entry(hub).or_default().entry(good).or_default() += n;
            self.dirty_buildings.insert(hub);
        }
    }

    /// Ware aus einem bestimmten Lager geholt (der Vorrat ist schon verringert).
    pub(super) fn hub_take(&mut self, hub: u32, good: Good, n: u32) {
        if let Some(have) = self.hub_goods.get_mut(&hub).and_then(|g| g.get_mut(&good)) {
            *have = have.saturating_sub(n);
            self.dirty_buildings.insert(hub);
        }
    }

    /// Wie viel liegt in diesem Lager?
    pub(super) fn hub_amount(&self, hub: u32, good: Good) -> u32 {
        self.hub_goods.get(&hub).and_then(|g| g.get(&good)).copied().unwrap_or(0)
    }

    /// Nächstes eigenes Lager, in dem die Ware tatsächlich liegt (sonst das nächste, das sie lagert).
    pub(super) fn nearest_hub_having(&self, owner: u32, p: (f32, f32), good: Good) -> Option<u32> {
        let dist = |b: &protocol::BuildingState| {
            let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
            let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
            dx * dx + dy * dy
        };
        self.buildings
            .iter()
            .filter(|b| b.owner == owner && self.economy.building(b.kind).is_some_and(|d| d.hub) && self.hub_amount(b.id, good) > 0)
            .min_by(|a, b| dist(a).total_cmp(&dist(b)))
            .map(|b| b.id)
            .or_else(|| self.nearest_hub(owner, p, good))
    }

    /// Nahrung (`population.food`) ohne Lager, das sie nimmt (kein Kornspeicher), verdirbt:
    /// der Bestand fällt auf 0. Andere Waren bleiben, auch ohne Lager.
    pub(super) fn spoil_unstored_food(&mut self) {
        let food = self.economy.def.population.food.clone();
        for (&player, stock) in self.stocks.iter_mut() {
            for &good in &food {
                let have = stock.amount(good);
                if have > 0 && !stock.has_place_for(good) {
                    stock.take_all(&[GoodAmount { good, amount: have }]);
                    self.dirty_stocks.insert(player);
                }
            }
        }
    }

    /// Bestand je Lager an den Gesamtvorrat angleichen.
    pub(super) fn balance_hubs(&mut self) {
        // (Id, Besitzer, Waren, die es nehmen darf: Auswahl des Spielers und Art des Lagers)
        let hubs: Vec<(u32, u32, Option<Vec<Good>>)> = self
            .buildings
            .iter()
            .filter(|b| self.economy.building(b.kind).is_some_and(|d| d.hub))
            .map(|b| (b.id, b.owner, self.hub_allowed(b)))
            .collect();
        let alive: BTreeSet<u32> = hubs.iter().map(|h| h.0).collect();
        let camps: BTreeSet<u32> = self.buildings.iter().filter(|b| b.kind == BuildingKind::SiegeCamp).map(|b| b.id).collect();
        self.hub_goods.retain(|id, _| alive.contains(id));
        let owners: Vec<u32> = self.stocks.keys().copied().collect();
        for owner in owners {
            let mine: Vec<&(u32, u32, Option<Vec<Good>>)> = hubs.iter().filter(|h| h.1 == owner).collect();
            if mine.is_empty() {
                continue;
            }
            let total: Vec<GoodAmount> = self.stocks[&owner].state().goods;
            let mut goods: BTreeSet<Good> = total.iter().map(|g| g.good).collect();
            for h in &mine {
                goods.extend(self.hub_goods.get(&h.0).into_iter().flat_map(|g| g.keys().copied()));
            }
            for good in goods {
                let want = total.iter().find(|g| g.good == good).map_or(0, |g| g.amount);
                let have: u32 = mine.iter().map(|h| self.hub_amount(h.0, good)).sum();
                // Belagerungslager bekommen nichts zugeteilt (nur, was Karren hinbringen).
                let stores = |h: &&&(u32, u32, Option<Vec<Good>>)| !camps.contains(&h.0) && h.2.as_ref().is_none_or(|s| s.contains(&good));
                if want > have {
                    // Erst dorthin, wo die Ware hin darf und Platz ist; Rest ins erste passende Lager.
                    let mut left = want - have;
                    for h in mine.iter().filter(stores) {
                        let n = self.hub_room(h.0, good).min(left);
                        self.hub_add(h.0, good, n);
                        left -= n;
                    }
                    // Rest ins erste passende Lager; nimmt keins die Ware (Essen ohne
                    // Kornspeicher, Waffen ohne Waffenkammer), bleibt sie im virtuellen Vorrat.
                    if let (true, Some(to)) = (left > 0, mine.iter().find(stores)) {
                        self.hub_add(to.0, good, left);
                    }
                } else if have > want {
                    let mut extra = have - want;
                    // Aus den Belagerungslagern zuletzt.
                    let mut order: Vec<(bool, u32, u32)> = mine.iter().map(|h| (!camps.contains(&h.0), self.hub_amount(h.0, good), h.0)).collect();
                    order.sort_by(|a, b| b.cmp(a));
                    let order: Vec<(u32, u32)> = order.into_iter().map(|(_, n, id)| (n, id)).collect();
                    for (n, id) in order {
                        let take = n.min(extra);
                        self.hub_take(id, good, take);
                        extra -= take;
                        if extra == 0 {
                            break;
                        }
                    }
                }
                // Volle Lager geben ab, was nicht mehr hineinpasst (Gesamtplatz je Lager).
                for h in &mine {
                    let over = self.hub_total(h.0).saturating_sub(self.hub_capacity(h.0)).min(self.hub_amount(h.0, good));
                    if over == 0 {
                        continue;
                    }
                    let mut left = over;
                    for o in mine.iter().filter(stores).filter(|o| o.0 != h.0) {
                        let n = self.hub_room(o.0, good).min(left);
                        self.hub_take(h.0, good, n);
                        self.hub_add(o.0, good, n);
                        left -= n;
                    }
                }
            }
        }
        for g in self.hub_goods.values_mut() {
            g.retain(|_, n| *n > 0);
        }
    }

    /// So viel passt insgesamt in dieses Lager: Vorratslager `storehouse_capacity`, Bergfried `base_capacity`.
    /// `hub_capacity` aus den Gebäudedaten, sonst wie früher.
    pub(super) fn hub_capacity(&self, hub: u32) -> u32 {
        let e = &self.economy.def;
        let Some(b) = self.buildings.iter().find(|b| b.id == hub) else { return 0 };
        match (self.economy.building(b.kind).map_or(0, |d| d.at(b.level).hub_capacity), b.kind) {
            (n, _) if n > 0 => n,
            (_, BuildingKind::Storehouse) => e.storehouse_capacity,
            _ => e.base_capacity,
        }
    }

    fn hub_room(&self, hub: u32, good: Good) -> u32 {
        let Some(b) = self.buildings.iter().find(|b| b.id == hub) else { return 0 };
        if !self.hub_accepts(b, good) {
            return 0;
        }
        self.hub_capacity(hub).saturating_sub(self.hub_total(hub))
    }

    /// Alles, was in diesem Lager liegt, zusammen.
    pub(super) fn hub_total(&self, hub: u32) -> u32 {
        self.hub_goods.get(&hub).map_or(0, |g| g.values().sum())
    }

    /// Welche Waren dieses Lager nimmt (Art des Lagers und Auswahl des Spielers); `None` = alle.
    pub(super) fn hub_allowed(&self, b: &protocol::BuildingState) -> Option<Vec<Good>> {
        let only = self.economy.building(b.kind).map(|d| d.hub_goods.clone()).unwrap_or_default();
        match (&b.settings.stores, only.is_empty()) {
            (s, true) => s.clone(),
            (None, false) => Some(only),
            (Some(s), false) => Some(s.iter().filter(|g| only.contains(g)).copied().collect()),
        }
    }

    /// Nimmt dieses Lager die Ware? Art des Lagers (`hub_goods`) und Auswahl des Spielers.
    pub(super) fn hub_accepts(&self, b: &protocol::BuildingState, good: Good) -> bool {
        let only = self.economy.building(b.kind).map(|d| &d.hub_goods);
        only.is_none_or(|o| o.is_empty() || o.contains(&good)) && b.settings.stores.as_ref().is_none_or(|s| s.contains(&good))
    }

    /// Inhalt eines Lagers für die Anzeige.
    pub(super) fn hub_state(&self, hub: u32) -> Option<Vec<GoodAmount>> {
        self.hub_goods.get(&hub).map(|g| g.iter().map(|(&good, &amount)| GoodAmount { good, amount }).collect())
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount};

    use crate::{economy::Economy, map::GameMap, world::World};

    #[test]
    fn jedes_lager_kennt_seinen_bestand() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        let keep = w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap().id;
        let store = w.place_building(1, BuildingKind::Storehouse, 20, 2).unwrap().id;
        w.step(0.05);
        let total = |w: &World, g: Good| w.hub_amount(keep, g) + w.hub_amount(store, g);
        assert_eq!(total(&w, Good::Wood), w.stocks[&1].amount(Good::Wood), "Summe = Vorrat");
        // Ein Träger legt im Vorratslager ab.
        w.stocks.get_mut(&1).unwrap().add(Good::Iron, 5);
        w.hub_add(store, Good::Iron, 5);
        w.step(0.05);
        assert_eq!(w.hub_amount(store, Good::Iron), 5);
        assert_eq!(w.hub_amount(keep, Good::Iron), 0);
        // Bauen nimmt vom Vorrat, der Bestand je Lager zieht nach.
        let before = total(&w, Good::Wood);
        w.place_building(1, BuildingKind::Woodcutter, 10, 12).unwrap();
        w.step(0.05);
        assert!(total(&w, Good::Wood) < before);
        assert_eq!(total(&w, Good::Wood), w.stocks[&1].amount(Good::Wood));
        // Das Lager zeigt seinen Inhalt im Gebäude an.
        let shown = w.buildings.iter().find(|b| b.id == store).unwrap().inventory.clone();
        assert!(shown.contains(&GoodAmount { good: Good::Iron, amount: 5 }), "{shown:?}");
        // Abriss: der Inhalt geht in das andere Lager.
        w.demolish(1, store).unwrap();
        w.step(0.05);
        assert_eq!(w.hub_amount(keep, Good::Iron), 5);
    }

    #[test]
    fn ohne_kornspeicher_verdirbt_nahrung() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap();
        w.stocks.get_mut(&1).unwrap().add_unlimited(Good::Bread, 30);
        w.stocks.get_mut(&1).unwrap().add_unlimited(Good::Iron, 5);
        w.step(0.05);
        assert_eq!(w.stocks[&1].amount(Good::Bread), 0, "Bergfried nimmt kein Essen");
        assert_eq!(w.stocks[&1].amount(Good::Iron), 5, "andere Waren bleiben");
        // Mit Kornspeicher bleibt das Essen.
        w.place_building(1, BuildingKind::Granary, 20, 2).unwrap();
        w.stocks.get_mut(&1).unwrap().add(Good::Bread, 10);
        w.step(0.05);
        assert_eq!(w.stocks[&1].amount(Good::Bread), 10);
    }

    #[test]
    fn erstes_lager_ist_gratis() {
        let row = "0".repeat(40);
        let text = format!("spawn 4 16\n{}", format!("{row}\n").repeat(20));
        // Mit Bergfried: dort ist Platz für die Erstattung beim Abriss.
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        // Welches Lager gratis ist, stellt Jan im Balance-Editor ein.
        let Some(kind) = w.economy.def.buildings.iter().find(|d| d.first_free && d.hub && d.kind != BuildingKind::Keep).map(|d| d.kind) else {
            return;
        };
        let before = w.stocks[&1].state().goods;
        let store = w.place_building(1, kind, 20, 2).unwrap().id;
        assert_eq!(w.stocks[&1].state().goods, before, "erstes Lager kostet nichts");
        w.place_building(1, kind, 20, 12).unwrap();
        let after = w.stocks[&1].state().goods;
        assert_ne!(after, before, "zweites Lager kostet");
        // Abriss erstattet wie üblich, solange noch eins bleibt.
        w.demolish(1, store).unwrap();
        assert_ne!(w.stocks[&1].state().goods, after);
    }

    /// Nimmt diese Lagerart die Ware laut economy.json (leere Liste = alles)?
    fn takes(w: &World, kind: BuildingKind, good: Good) -> bool {
        let only = &w.economy.building(kind).unwrap().hub_goods;
        only.is_empty() || only.contains(&good)
    }

    #[test]
    fn ohne_lager_kein_platz_startgueter_ziehen_ins_erste_lager() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        assert_eq!(w.stocks[&1].capacity, 0, "ohne Lager kein Platz");
        let wood = w.stocks[&1].amount(Good::Wood);
        assert!(wood > 0, "Startgüter liegen trotzdem bereit");
        let store = w.place_building(1, BuildingKind::Storehouse, 20, 2).unwrap().id;
        assert_eq!(w.stocks[&1].state().capacity, w.economy.def.storehouse_capacity, "Gesamtplatz");
        let food_in_store = takes(&w, BuildingKind::Storehouse, Good::Bread);
        assert_eq!(w.stocks[&1].room(Good::Bread) > 0, food_in_store, "Essen nur, wenn das Lager es nimmt");
        w.step(0.05);
        assert_eq!(w.hub_amount(store, Good::Wood), w.stocks[&1].amount(Good::Wood), "Startgüter im ersten Lager");
    }

    #[test]
    fn kornspeicher_nimmt_nur_lebensmittel() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }], 0);
        let store = w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap().id;
        let granary = w.place_building(1, BuildingKind::Granary, 20, 2).unwrap().id;
        let def = w.economy.building(BuildingKind::Granary).unwrap();
        let (cap, extra) = (w.economy.def.storehouse_capacity, def.at(1).hub_capacity);
        let food_in_store = takes(&w, BuildingKind::Storehouse, Good::Bread);
        let st = w.stocks[&1].state();
        assert_eq!(st.capacity, cap + extra, "Gesamtplatz beider Lager");
        let gp = st.pools.iter().find(|p| p.kind == BuildingKind::Granary).unwrap();
        assert_eq!(gp.capacity, extra);
        assert!(!gp.goods.contains(&Good::Wood) && gp.goods.contains(&Good::Bread), "Kornspeicher nur für Essen");
        let _ = food_in_store;
        w.admin_give(1, &[GoodAmount { good: Good::Bread, amount: 20 }, GoodAmount { good: Good::Wood, amount: 10 }], 0);
        w.step(0.05);
        assert_eq!(w.hub_amount(granary, Good::Wood), 0, "kein Holz im Kornspeicher");
        if food_in_store {
            assert!(w.hub_amount(granary, Good::Bread) + w.hub_amount(store, Good::Bread) >= 20, "Brot ist eingelagert");
        } else {
            assert!(w.hub_amount(granary, Good::Bread) >= 20, "Brot passt in den Kornspeicher");
            assert_eq!(w.hub_amount(store, Good::Bread), 0, "kein Brot im Vorratslager");
        }
    }

    #[test]
    fn volles_lager_gibt_an_das_naechste_ab() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        let keep = w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap().id;
        let store = w.place_building(1, BuildingKind::Storehouse, 20, 2).unwrap().id;
        let base = w.economy.def.base_capacity;
        w.admin_give(1, &[GoodAmount { good: Good::Iron, amount: base + 50 }], 0);
        w.step(0.05);
        assert_eq!(w.hub_total(keep), base, "Bergfried voll (Gesamtplatz)");
        // Nur Waren, die Bergfried oder Vorratslager nehmen (Brot gehört z. B. in den Kornspeicher).
        let takes = |g: Good| [BuildingKind::Keep, BuildingKind::Storehouse].iter().any(|&k| w.economy.building(k).unwrap().hub_goods.contains(&g));
        let all: u32 = w.stocks[&1].state().goods.iter().filter(|g| takes(g.good)).map(|g| g.amount).sum();
        assert_eq!(w.hub_total(keep) + w.hub_total(store), all, "Rest im Vorratslager");
        assert!(w.hub_amount(store, Good::Iron) > 0);
    }

    #[test]
    fn ausgebautes_lager_hat_mehr_platz_kostet_aber_mehr() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
        w.spawn_player(1);
        w.place_building(1, BuildingKind::Storehouse, 2, 2).unwrap();
        let lots: Vec<GoodAmount> = [Good::Wood, Good::Boards, Good::Stone].iter().map(|&good| GoodAmount { good, amount: 200 }).collect();
        w.admin_give(1, &lots, 0);
        for kind in [BuildingKind::Storehouse, BuildingKind::Armory] {
            let def = w.economy.building(kind).unwrap().clone();
            assert!(def.levels.len() >= 2, "{kind:?} ausbaubar");
            let sum = |l: &protocol::BuildingLevel| l.cost.iter().map(|c| c.amount).sum::<u32>();
            assert!(sum(&def.levels[1]) > sum(&def.levels[0]), "{kind:?}: Ausbau teurer als neu bauen");
            let x = if kind == BuildingKind::Storehouse { 20 } else { 30 };
            let id = w.place_building(1, kind, x, 2).unwrap().id;
            let before = w.hub_capacity(id);
            let total = w.stocks[&1].state().capacity;
            w.upgrade(1, id).unwrap();
            assert!(w.hub_capacity(id) > before, "{kind:?}: mehr Platz");
            assert_eq!(w.stocks[&1].state().capacity, total + w.hub_capacity(id) - before, "Gesamtplatz wächst gleich mit");
        }
    }
}
