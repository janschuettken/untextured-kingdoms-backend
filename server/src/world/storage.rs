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

    /// Bestand je Lager an den Gesamtvorrat angleichen.
    pub(super) fn balance_hubs(&mut self) {
        let hubs: Vec<(u32, u32, Option<Vec<Good>>)> = self
            .buildings
            .iter()
            .filter(|b| self.economy.building(b.kind).is_some_and(|d| d.hub))
            .map(|b| (b.id, b.owner, b.settings.stores.clone()))
            .collect();
        let alive: BTreeSet<u32> = hubs.iter().map(|h| h.0).collect();
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
                let stores = |h: &&&(u32, u32, Option<Vec<Good>>)| h.2.as_ref().is_none_or(|s| s.contains(&good));
                if want > have {
                    // Erst dorthin, wo die Ware hin darf und Platz ist; Rest ins erste passende Lager.
                    let mut left = want - have;
                    for h in mine.iter().filter(stores) {
                        let n = self.hub_room(h.0, good).min(left);
                        self.hub_add(h.0, good, n);
                        left -= n;
                    }
                    if left > 0 {
                        let to = mine.iter().find(stores).unwrap_or(&mine[0]).0;
                        self.hub_add(to, good, left);
                    }
                } else if have > want {
                    let mut extra = have - want;
                    let mut order: Vec<(u32, u32)> = mine.iter().map(|h| (self.hub_amount(h.0, good), h.0)).collect();
                    order.sort_by(|a, b| b.cmp(a));
                    for (n, id) in order {
                        let take = n.min(extra);
                        self.hub_take(id, good, take);
                        extra -= take;
                        if extra == 0 {
                            break;
                        }
                    }
                }
                // Volle Lager geben ab, was nicht mehr hineinpasst.
                for h in &mine {
                    let over = self.hub_amount(h.0, good).saturating_sub(self.hub_capacity(h.0));
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

    /// So viel passt je Ware in dieses Lager: Vorratslager `storehouse_capacity`, Bergfried `base_capacity`.
    fn hub_capacity(&self, hub: u32) -> u32 {
        let e = &self.economy.def;
        match self.buildings.iter().find(|b| b.id == hub).map(|b| b.kind) {
            Some(BuildingKind::Storehouse) => e.storehouse_capacity,
            _ => e.base_capacity,
        }
    }

    fn hub_room(&self, hub: u32, good: Good) -> u32 {
        self.hub_capacity(hub).saturating_sub(self.hub_amount(hub, good))
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
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        let keep = w.place_building(1, BuildingKind::Keep, 2, 2).unwrap().id;
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
    fn erstes_lager_ist_gratis() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        let before = w.stocks[&1].state().goods;
        let store = w.place_building(1, BuildingKind::Storehouse, 20, 2).unwrap().id;
        assert_eq!(w.stocks[&1].state().goods, before, "erstes Lager kostet nichts");
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        assert_ne!(w.stocks[&1].state().goods, before, "zweites Lager kostet");
        // Abriss erstattet wie üblich, solange ein anderes Lager bleibt.
        w.demolish(1, store).unwrap();
    }

    #[test]
    fn volles_lager_gibt_an_das_naechste_ab() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        let keep = w.place_building(1, BuildingKind::Keep, 2, 2).unwrap().id;
        let store = w.place_building(1, BuildingKind::Storehouse, 20, 2).unwrap().id;
        let base = w.economy.def.base_capacity;
        w.admin_give(1, &[GoodAmount { good: Good::Iron, amount: base + 50 }], 0);
        w.step(0.05);
        assert_eq!(w.hub_amount(keep, Good::Iron), base, "Bergfried voll");
        assert_eq!(w.hub_amount(store, Good::Iron), 50, "Rest im Vorratslager");
    }
}
