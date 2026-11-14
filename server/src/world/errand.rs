//! Botengänge: Arbeiter eines Betriebs (Sägewerk, Bäckerei, ...) bedienen ihr
//! eigenes Gebäude selbst, solange der Spieler keine Träger hat.
//!
//! Träger und Karren gibt es nur noch im Wagenschuppen. Ohne sie trägt der
//! Arbeiter fertige Waren selbst ins nächste Lager und holt fehlende Eingänge
//! selbst von dort. In der Zeit steht der Betrieb (`WorkerAway`).

use protocol::GoodAmount;

use super::{gather::Gather, World};

/// Wie lange ein Arbeiter wartet, bevor er wieder nach Arbeit schaut.
const WAIT: f32 = 1.0;
/// So nah (Felder vom Gebäuderand) muss er kommen, um zu laden.
const REACH: f32 = 1.6;

impl World {
    /// Hat der Spieler Träger oder Karren, die arbeiten (also eine Wohnung haben)?
    pub(super) fn has_transport(&self, owner: u32) -> bool {
        self.units.iter().any(|u| u.owner == owner && u.is_transport() && !self.idle_units.contains(&u.id))
    }

    /// Ein Schritt für einen Arbeiter eines Herstellungsbetriebs.
    /// Liefert, ob er gerade an seinem Betrieb ist.
    pub(super) fn step_errand(&mut self, k: usize, bi: usize, dt: f32) -> bool {
        if !self.units[k].path.is_empty() {
            return false;
        }
        let owner = self.buildings[bi].owner;
        match self.units[k].gather {
            Some(Gather::Deliver(hub)) => {
                if self.near(k, hub) {
                    if let Some(load) = self.units[k].carrying.take() {
                        let stock = self.stocks.get_mut(&owner).expect("Spieler hat einen Vorrat");
                        let fits = stock.add(load.good, load.amount);
                        self.dirty_stocks.insert(owner);
                        if fits < load.amount {
                            // Passt nicht ganz: Rest wieder mit zurücknehmen.
                            self.units[k].carrying = Some(GoodAmount { good: load.good, amount: load.amount - fits });
                        }
                    }
                }
                self.walk_home(k, bi);
                false
            }
            Some(Gather::Fetch(hub)) => {
                if self.near(k, hub) {
                    if let Some(load) = self.missing_input(bi) {
                        let stock = self.stocks.get_mut(&owner).expect("Spieler hat einen Vorrat");
                        let got = load.amount.min(stock.amount(load.good));
                        if got > 0 && stock.take_all(&[GoodAmount { good: load.good, amount: got }]) {
                            self.dirty_stocks.insert(owner);
                            self.units[k].carrying = Some(GoodAmount { good: load.good, amount: got });
                        }
                    }
                }
                self.walk_home(k, bi);
                false
            }
            Some(Gather::Home) => {
                let id = self.buildings[bi].id;
                if let Some(load) = self.units[k].carrying {
                    let inv = self.inventories.get_mut(&id).expect("Betrieb hat ein Lager");
                    let fits = load.amount.min(inv.capacity.saturating_sub(inv.amount(load.good)));
                    inv.add(load.good, fits);
                    self.dirty_buildings.insert(id);
                    // Was nicht passt, geht in den Vorrat, damit nichts verloren geht.
                    if fits < load.amount {
                        if let Some(stock) = self.stocks.get_mut(&owner) {
                            stock.add(load.good, load.amount - fits);
                            self.dirty_stocks.insert(owner);
                        }
                    }
                    self.units[k].carrying = None;
                }
                self.units[k].gather = None;
                self.units[k].cooldown = 0.0;
                true
            }
            Some(Gather::ToTile(_) | Gather::Working { .. }) | None => {
                self.units[k].gather = None;
                let u = &mut self.units[k];
                u.cooldown -= dt;
                if u.cooldown > 0.0 {
                    return true;
                }
                u.cooldown = WAIT;
                let b = &self.buildings[bi];
                if b.settings.paused || self.unstaffed.contains(&b.id) || self.has_transport(owner) {
                    return true;
                }
                self.start_errand(k, bi)
            }
        }
    }

    /// Losgehen, wenn es etwas zu tragen gibt: erst Fertiges abliefern, dann Eingänge holen.
    fn start_errand(&mut self, k: usize, bi: usize) -> bool {
        let b = &self.buildings[bi];
        let (id, owner) = (b.id, b.owner);
        let cap = self.economy.def.transport.carrier_capacity;
        let inputs: Vec<GoodAmount> =
            b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).map_or_else(Vec::new, |r| r.inputs.clone());
        let pos = self.units[k].pos;
        let Some(inv) = self.inventories.get(&id) else { return true };
        let stock = self.stocks.get(&owner);
        // Abliefern: alles, was kein Eingang ist.
        let out = inv.state().goods.into_iter().find_map(|g| {
            if inputs.iter().any(|i| i.good == g.good) {
                return None;
            }
            let room = stock.map_or(0, |s| s.capacity.saturating_sub(s.amount(g.good)));
            let amount = g.amount.min(cap).min(room);
            let hub = self.nearest_hub(owner, pos, g.good)?;
            (amount > 0).then_some((hub, GoodAmount { good: g.good, amount }))
        });
        if let Some((hub, load)) = out {
            if self.walk_to(k, hub) {
                let inv = self.inventories.get_mut(&id).expect("eben gefunden");
                inv.take_all(&[load]);
                self.dirty_buildings.insert(id);
                self.units[k].carrying = Some(load);
                self.units[k].gather = Some(Gather::Deliver(hub));
                return false;
            }
        }
        // Holen: Eingänge für zwei Durchläufe, wenn der Betrieb Lieferungen annimmt.
        if !self.buildings[bi].settings.accept_deliveries {
            return true;
        }
        if let Some(load) = self.missing_input(bi) {
            if let Some(hub) = self.nearest_hub(owner, pos, load.good) {
                if self.walk_to(k, hub) {
                    self.units[k].gather = Some(Gather::Fetch(hub));
                    return false;
                }
            }
        }
        true
    }

    /// Der Eingang, der am meisten fehlt und im Vorrat liegt (Mindestbestand bleibt).
    fn missing_input(&self, bi: usize) -> Option<GoodAmount> {
        let b = &self.buildings[bi];
        let recipe = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r))?;
        let inv = self.inventories.get(&b.id)?;
        let stock = self.stocks.get(&b.owner)?;
        let cap = self.economy.def.transport.carrier_capacity;
        recipe
            .inputs
            .iter()
            .filter_map(|i| {
                let want = (i.amount * 2).min(inv.capacity);
                let keep = b.settings.keep_in_stock.iter().find(|g| g.good == i.good).map_or(0, |g| g.amount);
                let there = stock.amount(i.good).saturating_sub(keep);
                let amount = want.saturating_sub(inv.amount(i.good)).min(cap).min(there);
                (amount > 0).then_some(GoodAmount { good: i.good, amount })
            })
            .max_by_key(|g| g.amount)
    }

    fn near(&self, k: usize, building: u32) -> bool {
        self.buildings.iter().find(|b| b.id == building).is_some_and(|b| {
            let p = self.units[k].pos;
            let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
            let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
            (dx * dx + dy * dy).sqrt() <= REACH
        })
    }

    /// Zum Gebäude laufen. `false`, wenn es keinen freien Platz davor gibt.
    fn walk_to(&mut self, k: usize, building: u32) -> bool {
        let pos = self.units[k].pos;
        let Some(b) = self.buildings.iter().find(|b| b.id == building) else { return false };
        let Some(door) = self.door(b, pos) else { return false };
        let u = &mut self.units[k];
        u.path = self.map.nav.waypoints(pos, door);
        u.target = Some(door);
        true
    }

    fn walk_home(&mut self, k: usize, bi: usize) {
        let id = self.buildings[bi].id;
        self.units[k].gather = Some(Gather::Home);
        if !self.near(k, id) {
            self.walk_to(k, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, BuildingStatus, Good, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn flat() -> World {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        w
    }

    #[test]
    fn ohne_wagenschuppen_traegt_der_arbeiter_selbst() {
        let mut w = flat();
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 25, 2).unwrap();
        assert_eq!(w.units.iter().filter(|u| u.kind == UnitKind::Carrier).count(), 0);
        let worker = w.units.iter().position(|u| u.kind == UnitKind::Worker && u.home == Some(saw.id)).unwrap();
        let (wood, boards) = (w.stocks[&1].amount(Good::Wood), w.stocks[&1].amount(Good::Boards));
        let mut away = false;
        let mut carried = false;
        for _ in 0..20 * 120 {
            w.step(0.05);
            let b = w.buildings.iter().find(|b| b.id == saw.id).unwrap();
            away |= b.status == BuildingStatus::WorkerAway;
            carried |= w.units[worker].carrying.is_some_and(|g| g.good == Good::Wood);
        }
        assert!(away, "Betrieb steht, während der Arbeiter unterwegs ist");
        assert!(carried, "Arbeiter holt Holz selbst");
        assert!(w.stocks[&1].amount(Good::Wood) < wood);
        assert!(w.stocks[&1].amount(Good::Boards) > boards, "Bretter selbst ins Lager gebracht");
    }

    #[test]
    fn mit_traegern_bleibt_der_arbeiter_am_betrieb() {
        let mut w = flat();
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        w.place_building(1, BuildingKind::CartShed, 12, 12).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 25, 2).unwrap();
        let worker = w.units.iter().position(|u| u.kind == UnitKind::Worker && u.home == Some(saw.id)).unwrap();
        let boards = w.stocks[&1].amount(Good::Boards);
        for _ in 0..20 * 90 {
            w.step(0.05);
            assert!(w.units[worker].carrying.is_none(), "Träger tragen, nicht der Arbeiter");
        }
        assert!(w.stocks[&1].amount(Good::Boards) > boards);
    }
}
