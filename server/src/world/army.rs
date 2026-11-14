//! Heer: Die Kaserne bildet Soldaten aus Ausrüstung und Talern aus, und an
//! jedem Zahltag bekommen alle Soldaten ihren Sold.
//!
//! Die Warteschlange einer Kaserne steht direkt in `BuildingState::training`
//! (so wird sie mitgespeichert und geht an die Clients). Bezahlt wird beim
//! Auftrag; wer einen Auftrag streicht oder die Kaserne abreißt, bekommt die
//! Kosten der noch nicht fertigen Soldaten ganz zurück.

use protocol::{ArmyState, BuildingState, GoodAmount, Text, UnitKind};

use super::{Refusal, Unit, World};

/// Name einer Ware wie im Protokoll, für Text-Platzhalter.
fn good_key(g: protocol::Good) -> String {
    serde_json::to_value(g).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

impl World {
    /// Soldaten, die diese Kaserne auf ihrer Stufe ausbilden kann.
    fn trainable(&self, b: &BuildingState) -> Vec<UnitKind> {
        self.economy
            .building(b.kind)
            .map(|d| d.levels.iter().take(b.level as usize).flat_map(|l| l.trains.iter().copied()).collect())
            .unwrap_or_default()
    }

    /// `count` Soldaten in Auftrag geben. Bezahlt wird sofort; reicht es nicht
    /// für alle, kommen so viele in die Warteschlange, wie bezahlbar sind.
    pub fn train(&mut self, player: u32, building_id: u32, kind: UnitKind, count: u32) -> Result<BuildingState, Refusal> {
        let b = self.buildings.iter().find(|b| b.id == building_id).ok_or_else(|| Text::new("error.no_building"))?;
        if b.owner != player {
            return Err(Text::new("error.not_yours"));
        }
        if !self.trainable(b).contains(&kind) {
            return Err(Text::new("error.cannot_train"));
        }
        let unit = self.economy.unit(kind).ok_or_else(|| Text::new("error.cannot_train"))?.clone();
        let max = self.economy.def.army.queue_max;
        let room = max.saturating_sub(b.training.len() as u32);
        if room == 0 {
            return Err(Text::new("error.queue_full").arg("max", max));
        }
        let stock = self.stocks.get_mut(&player).ok_or_else(|| Text::new("error.no_player"))?;
        let mut ordered = 0;
        for _ in 0..count.clamp(1, room) {
            if let Some(missing) = unit.cost.iter().find(|c| stock.amount(c.good) < c.amount) {
                if ordered > 0 {
                    break;
                }
                return Err(Text::new("error.not_enough")
                    .arg("good", good_key(missing.good))
                    .arg("need", missing.amount - stock.amount(missing.good)));
            }
            if stock.taler < unit.taler {
                if ordered > 0 {
                    break;
                }
                return Err(Text::new("error.not_enough_taler").arg("need", unit.taler - stock.taler));
            }
            stock.take_all(&unit.cost);
            stock.taler -= unit.taler;
            ordered += 1;
        }
        self.dirty_stocks.insert(player);
        let b = self.buildings.iter_mut().find(|b| b.id == building_id).expect("eben gefunden");
        if b.training.is_empty() {
            b.training_left = unit.seconds;
        }
        b.training.extend(std::iter::repeat_n(kind, ordered as usize));
        Ok(b.clone())
    }

    /// Den zuletzt bestellten Soldaten streichen und die Kosten erstatten.
    pub fn cancel_training(&mut self, player: u32, building_id: u32) -> Result<BuildingState, Refusal> {
        let b = self.buildings.iter_mut().find(|b| b.id == building_id).ok_or_else(|| Text::new("error.no_building"))?;
        if b.owner != player {
            return Err(Text::new("error.not_yours"));
        }
        let Some(kind) = b.training.pop() else {
            return Err(Text::new("error.nothing_to_cancel"));
        };
        if b.training.is_empty() {
            b.training_left = 0.0;
        }
        let b = b.clone();
        self.refund_training(player, &[kind]);
        Ok(b)
    }

    /// Kosten nicht fertiger Soldaten zurück in den Vorrat.
    pub(super) fn refund_training(&mut self, player: u32, kinds: &[UnitKind]) {
        let mut goods: Vec<GoodAmount> = Vec::new();
        let mut taler = 0;
        for k in kinds {
            if let Some(u) = self.economy.unit(*k) {
                goods.extend(u.cost.iter().copied());
                taler += u.taler;
            }
        }
        let Some(stock) = self.stocks.get_mut(&player) else { return };
        // Das kam eben erst aus dem Vorrat, also auch ohne Platz zurück.
        for g in goods {
            stock.add_unlimited(g.good, g.amount);
        }
        stock.taler = stock.taler.saturating_add(taler);
        self.dirty_stocks.insert(player);
    }

    /// Ausbildung voranbringen, fertige Soldaten vor die Tür stellen, Zahltag.
    pub(super) fn step_army(&mut self, dt: f32) {
        let mut done = Vec::new();
        for b in self.buildings.iter_mut().filter(|b| !b.training.is_empty()) {
            b.training_left -= dt;
            if b.training_left <= 0.0 {
                done.push(b.id);
            }
        }
        for id in done {
            let Some(b) = self.buildings.iter().find(|b| b.id == id).cloned() else { continue };
            if !self.spawn_soldier(&b, b.training[0]) {
                // Tür zugebaut: warten, bis wieder Platz ist.
                continue;
            }
            let b = self.buildings.iter_mut().find(|x| x.id == id).expect("eben gefunden");
            b.training.remove(0);
            b.training_left = b.training.first().and_then(|k| self.economy.unit(*k)).map_or(0.0, |u| u.seconds);
            self.dirty_buildings.insert(id);
            self.dirty_stocks.insert(b.owner);
        }
        self.step_payday(dt);
    }

    /// Neuer Soldat vor der Kaserne. Er tritt ein paar Felder vor die Tür,
    /// damit die nächsten nicht im Eingang stehen.
    fn spawn_soldier(&mut self, b: &BuildingState, kind: UnitKind) -> bool {
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let Some(door) = self.door(b, center) else { return false };
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        self.units.push(Unit::new(id, b.owner, kind, door));
        // Sammelplatz: Reihen zu dreien vor dem Eingang.
        let (out, side) = match b.rotation % 4 {
            0 => ((0.0, 1.0), (1.0, 0.0)),
            1 => ((-1.0, 0.0), (0.0, 1.0)),
            2 => ((0.0, -1.0), (-1.0, 0.0)),
            _ => ((1.0, 0.0), (0.0, -1.0)),
        };
        let near = self
            .units
            .iter()
            .filter(|u| u.owner == b.owner && u.kind.is_soldier() && u.path.is_empty())
            .filter(|u| (u.pos.0 - door.0).abs() + (u.pos.1 - door.1).abs() < 8.0)
            .count() as i32;
        let n = near % 9;
        let (row, col) = ((2 + n / 3) as f32, (n % 3 - 1) as f32);
        let spot = (door.0 + out.0 * row + side.0 * col, door.1 + out.1 * row + side.1 * col);
        if self.map.nav.is_open(spot.0.floor() as i32, spot.1.floor() as i32) {
            self.order_move(b.owner, &[id], spot);
        }
        true
    }

    /// Alle `upkeep_seconds` bekommt jeder Soldat seinen Sold. Reicht das Geld
    /// nicht, wird alles Geld verteilt und die Soldaten laufen langsamer.
    fn step_payday(&mut self, dt: f32) {
        let every = self.economy.def.army.upkeep_seconds;
        if every <= 0.0 {
            self.unpaid.clear();
            return;
        }
        let players: Vec<u32> = self.stocks.keys().copied().collect();
        for p in players {
            let timer = self.pay_timer.entry(p).or_insert(0.0);
            *timer += dt;
            if *timer < every {
                continue;
            }
            *timer -= every;
            let upkeep = self.army_state(p).upkeep;
            if upkeep == 0 {
                self.unpaid.remove(&p);
                continue;
            }
            let stock = self.stocks.get_mut(&p).expect("Spieler hat Vorrat");
            let was = self.unpaid.contains(&p);
            if stock.taler >= upkeep {
                stock.taler -= upkeep;
                self.unpaid.remove(&p);
            } else {
                stock.taler = 0;
                self.unpaid.insert(p);
            }
            if was != self.unpaid.contains(&p) || upkeep > 0 {
                self.dirty_stocks.insert(p);
            }
        }
    }

    /// Soldaten, Sold und ob er zuletzt gereicht hat.
    pub(super) fn army_state(&self, player: u32) -> ArmyState {
        let mut s = ArmyState { soldiers: 0, upkeep: 0, unpaid: self.unpaid.contains(&player) };
        for u in self.units.iter().filter(|u| u.owner == player && u.kind.is_soldier()) {
            s.soldiers += 1;
            s.upkeep += self.economy.unit(u.kind).map_or(0, |d| d.upkeep);
        }
        s
    }

    /// Tempo eines Soldaten (langsamer, wenn der Sold ausbleibt).
    pub(super) fn soldier_speed(&self, owner: u32, kind: UnitKind) -> f32 {
        let base = self.economy.unit(kind).map_or(1.0, |u| u.speed);
        if self.unpaid.contains(&owner) {
            base * self.economy.def.army.unpaid_speed_percent as f32 / 100.0
        } else {
            base
        }
    }

    /// Kasernen dieses Spielers (für Tests und Anzeige).
    #[cfg(test)]
    fn barracks(&self, player: u32) -> Vec<&BuildingState> {
        self.buildings.iter().filter(|b| b.owner == player && b.kind == protocol::BuildingKind::Barracks).collect()
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn world() -> World {
        let row = "0".repeat(30);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(30));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w
    }

    fn soldiers(w: &World, kind: UnitKind) -> usize {
        w.units.iter().filter(|u| u.kind == kind).count()
    }

    #[test]
    fn kaserne_bildet_speertraeger_aus() {
        let mut w = world();
        let b = w.place_building(1, BuildingKind::Barracks, 10, 10).unwrap();
        assert_eq!(w.train(1, b.id, UnitKind::Spearman, 1).unwrap_err().key, "error.not_enough");
        w.admin_give(1, &[GoodAmount { good: Good::Spear, amount: 3 }], 0);
        let taler = w.stock_state(1).taler;
        let b2 = w.train(1, b.id, UnitKind::Spearman, 5).unwrap();
        assert_eq!(b2.training.len(), 3, "nur so viele, wie Speere da sind");
        assert_eq!(w.stock_state(1).taler, taler - 30);
        assert_eq!(w.train(1, b.id, UnitKind::Swordsman, 1).unwrap_err().key, "error.cannot_train", "erst Stufe 2");
        assert_eq!(w.train(2, b.id, UnitKind::Spearman, 1).unwrap_err().key, "error.not_yours");

        // Einen streichen: Speer und Taler zurück.
        w.cancel_training(1, b.id).unwrap();
        assert_eq!(w.stock_state(1).taler, taler - 20);
        assert_eq!(w.stock_state(1).goods.iter().find(|g| g.good == Good::Spear).unwrap().amount, 1);

        for _ in 0..20 * 45 {
            w.step(0.05);
        }
        assert_eq!(soldiers(&w, UnitKind::Spearman), 2);
        assert!(w.barracks(1)[0].training.is_empty());
        let army = w.stock_state(1).army;
        assert_eq!((army.soldiers, army.upkeep), (2, 2));
        // Soldaten stehen außerhalb der Kaserne auf begehbarem Boden und gehorchen.
        for u in w.units.iter().filter(|u| u.kind == UnitKind::Spearman) {
            assert!(w.map.nav.is_open(u.pos.0 as i32, u.pos.1 as i32));
        }
        let id = w.units.iter().find(|u| u.kind == UnitKind::Spearman).unwrap().id;
        w.order_move(1, &[id], (2.5, 2.5));
        for _ in 0..20 * 30 {
            w.step(0.05);
        }
        let u = w.units.iter().find(|u| u.id == id).unwrap();
        assert!((u.pos.0 - 2.5).abs() < 1e-3 && (u.pos.1 - 2.5).abs() < 1e-3);
    }

    #[test]
    fn warteschlange_ist_begrenzt_und_abriss_erstattet() {
        let mut w = world();
        let b = w.place_building(1, BuildingKind::Barracks, 10, 10).unwrap();
        w.admin_give(1, &[GoodAmount { good: Good::Spear, amount: 20 }], 1000);
        let max = w.economy.def.army.queue_max as usize;
        assert_eq!(w.train(1, b.id, UnitKind::Spearman, 50).unwrap().training.len(), max);
        assert_eq!(w.train(1, b.id, UnitKind::Spearman, 1).unwrap_err().key, "error.queue_full");
        let before = w.stock_state(1).taler;
        w.demolish(1, b.id).unwrap();
        assert_eq!(w.stock_state(1).taler, before + 10 * max as u32);
    }

    #[test]
    fn zahltag_ohne_geld_macht_langsam() {
        let mut w = world();
        let b = w.place_building(1, BuildingKind::Barracks, 10, 10).unwrap();
        w.admin_give(1, &[GoodAmount { good: Good::Spear, amount: 1 }], 0);
        w.train(1, b.id, UnitKind::Spearman, 1).unwrap();
        for _ in 0..20 * 25 {
            w.step(0.05);
        }
        assert_eq!(soldiers(&w, UnitKind::Spearman), 1);
        w.stocks.get_mut(&1).unwrap().taler = 0;
        let every = w.economy.def.army.upkeep_seconds;
        for _ in 0..(every * 20.0) as usize + 1 {
            w.step(0.05);
        }
        assert!(w.stock_state(1).army.unpaid);
        let full = w.economy.unit(UnitKind::Spearman).unwrap().speed;
        assert!(w.soldier_speed(1, UnitKind::Spearman) < full);
        w.admin_give(1, &[], 100);
        for _ in 0..(every * 20.0) as usize + 1 {
            w.step(0.05);
        }
        assert!(!w.stock_state(1).army.unpaid);
        assert_eq!(w.soldier_speed(1, UnitKind::Spearman), full);
    }
}
