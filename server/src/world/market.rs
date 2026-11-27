//! Markt: Waren gegen Taler kaufen und verkaufen.
//!
//! Ein gemeinsamer Markt für alle. Grundpreise aus `market.prices`; jedes gekaufte Stück
//! hebt den Preis um `price_step_percent`, jedes verkaufte senkt ihn, und mit der Zeit
//! holen die Preise wieder zum Grundpreis auf (`price_recovery_percent` je Minute).
//! Handeln darf, wer einen eigenen Markt mit Arbeiter hat; die Ware geht sofort
//! in den Vorrat bzw. aus ihm heraus. Taler kommen sonst aus Steuern
//! (`market.tax_per_meal` je satter Bewohner und Mahlzeit, siehe `people.rs`).

use protocol::{BuildingKind, Good, GoodAmount, Price, Text};

use super::{Refusal, World};

/// Alle so viele Sekunden (Spielzeit) wird automatisch gehandelt.
const AUTO_TRADE_SECONDS: f32 = 5.0;

/// Preis bei diesem Faktor: gerundet, aber nie unter 1 Taler (0 bleibt 0 = nicht handelbar).
fn scaled(base: u32, factor: f32) -> u32 {
    if base == 0 {
        0
    } else {
        ((base as f32 * factor).round() as u32).max(1)
    }
}

impl World {
    fn price_factor(&self, good: Good) -> f32 {
        self.price_factor.get(&good).copied().unwrap_or(1.0)
    }

    /// Faktor nach einem gekauften (`sell` = false) oder verkauften Stück.
    fn next_factor(&self, factor: f32, sell: bool) -> f32 {
        let m = &self.economy.def.market;
        let step = 1.0 + m.price_step_percent.max(0.0) / 100.0;
        let f = if sell { factor / step } else { factor * step };
        f.clamp(m.price_min_percent.max(1.0) / 100.0, m.price_max_percent.max(m.price_min_percent.max(1.0)) / 100.0)
    }

    /// Aktueller Preis einer Ware (Grundpreis mal Faktor der Preisdynamik).
    pub fn price_now(&self, good: Good) -> Option<Price> {
        let base = self.economy.def.market.prices.iter().find(|p| p.good == good)?;
        let f = self.price_factor(good);
        Some(Price { good, buy: scaled(base.buy, f), sell: scaled(base.sell, f) })
    }

    /// Alle aktuellen Preise (für die Anzeige).
    pub fn current_prices(&self) -> Vec<Price> {
        self.economy.def.market.prices.iter().filter_map(|p| self.price_now(p.good)).collect()
    }

    /// Was `amount` Stück zusammen kosten (Kauf) oder bringen (Verkauf), Stück für Stück
    /// mit wanderndem Preis, und der Faktor danach.
    fn quote(&self, good: Good, amount: u32, sell: bool) -> Option<(u32, f32)> {
        let base = *self.economy.def.market.prices.iter().find(|p| p.good == good)?;
        let mut f = self.price_factor(good);
        let mut total = 0u32;
        for _ in 0..amount {
            total = total.saturating_add(scaled(if sell { base.sell } else { base.buy }, f));
            f = self.next_factor(f, sell);
        }
        Some((total, f))
    }

    /// Wie viele Stück man mit `taler` höchstens kaufen kann (bis `max`).
    fn affordable(&self, good: Good, taler: u32, max: u32) -> u32 {
        let Some(base) = self.economy.def.market.prices.iter().find(|p| p.good == good).copied() else { return 0 };
        let mut f = self.price_factor(good);
        let (mut left, mut n) = (taler, 0);
        while n < max {
            let p = scaled(base.buy, f);
            if p > left {
                break;
            }
            left -= p;
            n += 1;
            f = self.next_factor(f, false);
        }
        n
    }

    /// Preise holen mit der Zeit zum Grundpreis auf.
    fn recover_prices(&mut self, dt: f32) {
        let pct = self.economy.def.market.price_recovery_percent.clamp(0.0, 100.0);
        if self.price_factor.is_empty() || pct <= 0.0 {
            return;
        }
        let keep = if pct >= 100.0 { 0.0 } else { (1.0 - pct / 100.0).powf(dt / 60.0) };
        self.price_factor.retain(|_, f| {
            *f = 1.0 + (*f - 1.0) * keep;
            (*f - 1.0).abs() > 1e-3
        });
    }

    /// Haben sich die gerundeten Preise geändert, bekommen alle Spieler den neuen Stand.
    fn publish_prices(&mut self) {
        let now = self.current_prices();
        if now != self.sent_prices {
            self.sent_prices = now;
            let players: Vec<u32> = self.stocks.keys().copied().collect();
            self.dirty_stocks.extend(players);
        }
    }

    /// Automatischen Handel für eine Ware einstellen (0 = aus).
    pub fn set_auto_trade(&mut self, player: u32, good: Good, buy_below: u32, sell_above: u32) -> Result<(), Refusal> {
        if !self.economy.def.market.prices.iter().any(|p| p.good == good) {
            return Err(Text::new("error.not_traded"));
        }
        if buy_below > 0 && sell_above > 0 && sell_above <= buy_below {
            return Err(Text::new("error.auto_trade_range"));
        }
        let rules = self.auto_trade.entry(player).or_default();
        rules.retain(|r| r.good != good);
        if buy_below > 0 || sell_above > 0 {
            rules.push(protocol::AutoTrade { good, buy_below, sell_above });
            rules.sort_by_key(|r| r.good);
        }
        self.dirty_stocks.insert(player);
        Ok(())
    }

    /// Automatisch kaufen und verkaufen, was eingestellt ist (braucht einen Markt mit Arbeiter).
    pub(super) fn step_auto_trade(&mut self, dt: f32) {
        self.recover_prices(dt);
        self.auto_trade_clock += dt;
        if self.auto_trade_clock < AUTO_TRADE_SECONDS {
            return;
        }
        self.auto_trade_clock = 0.0;
        self.publish_prices();
        let rules: Vec<(u32, protocol::AutoTrade)> = self.auto_trade.iter().flat_map(|(&p, r)| r.iter().map(move |r| (p, *r))).collect();
        for (player, r) in rules {
            if self.eliminated.contains(&player) {
                continue;
            }
            let Some(stock) = self.stocks.get(&player) else { continue };
            let have = stock.amount(r.good);
            if r.sell_above > 0 && have > r.sell_above {
                let _ = self.trade(player, r.good, have - r.sell_above, true);
            } else if r.buy_below > 0 && have < r.buy_below {
                let want = (r.buy_below - have).min(stock.room(r.good));
                let amount = self.affordable(r.good, stock.taler, want);
                if amount > 0 {
                    let _ = self.trade(player, r.good, amount, false);
                }
            }
        }
    }

    pub fn trade(&mut self, player: u32, good: Good, amount: u32, sell: bool) -> Result<(), Refusal> {
        if amount == 0 {
            return Ok(());
        }
        let markets: Vec<u32> = self.buildings.iter().filter(|b| b.owner == player && b.kind == BuildingKind::Market).map(|b| b.id).collect();
        if markets.is_empty() {
            return Err(Text::new("error.no_market"));
        }
        if markets.iter().all(|id| self.unstaffed.contains(id)) {
            return Err(Text::new("error.market_no_workers"));
        }
        let base = self.economy.def.market.prices.iter().find(|p| p.good == good).copied().ok_or_else(|| Text::new("error.not_traded"))?;
        let room = self.stocks.get(&player).ok_or_else(|| Text::new("error.no_player"))?.room(good);
        let (total, factor) = self.quote(good, if sell { amount } else { amount.min(room) }, sell).expect("eben gefunden");
        let stock = self.stocks.get_mut(&player).ok_or_else(|| Text::new("error.no_player"))?;
        if sell {
            // Verkaufspreis 0: die Ware nimmt der Markt nicht an (z. B. Heilsalbe).
            if base.sell == 0 {
                return Err(Text::new("error.not_sellable"));
            }
            if stock.amount(good) < amount {
                return Err(Text::new("error.not_enough").arg("good", good_key(good)).arg("need", amount - stock.amount(good)));
            }
            stock.take_all(&[GoodAmount { good, amount }]);
            stock.taler = stock.taler.saturating_add(total);
        } else {
            let amount = amount.min(room);
            if amount == 0 {
                return Err(Text::new("error.storage_full"));
            }
            let cost = total;
            if stock.taler < cost {
                return Err(Text::new("error.not_enough_taler").arg("need", cost - stock.taler));
            }
            stock.taler -= cost;
            stock.add(good, amount);
        }
        self.dirty_stocks.insert(player);
        if (factor - 1.0).abs() > 1e-3 {
            self.price_factor.insert(good, factor);
        } else {
            self.price_factor.remove(&good);
        }
        self.publish_prices();
        Ok(())
    }
}

fn good_key(good: Good) -> String {
    serde_json::to_value(good).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount};

    use crate::{economy::Economy, map::GameMap, world::World};

    #[test]
    fn kaufen_und_verkaufen() {
        let row = "0".repeat(30);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(30));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.economy.def.market.price_step_percent = 0.0; // feste Preise (Dynamik: eigener Test)
        w.spawn_player(1);
        let price = *w.economy.def.market.prices.iter().find(|p| p.good == Good::Bread).unwrap();
        assert_eq!(w.trade(1, Good::Bread, 1, true).unwrap_err().key, "error.no_market");
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }, GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::Market, 5, 5).unwrap();
        w.place_building(1, BuildingKind::Granary, 15, 15).unwrap(); // Platz für gekauftes Brot
        let taler = w.stock_state(1).taler;
        w.trade(1, Good::Bread, 10, true).unwrap();
        assert_eq!(w.stock_state(1).taler, taler + 10 * price.sell);
        w.trade(1, Good::Bread, 2, false).unwrap();
        assert_eq!(w.stock_state(1).taler, taler + 10 * price.sell - 2 * price.buy);
        assert_eq!(w.trade(1, Good::Bread, 280, false).unwrap_err().key, "error.not_enough_taler");
        assert_eq!(w.trade(1, Good::Bread, 1000, true).unwrap_err().key, "error.not_enough");

        // Automatisch: über 5 verkaufen, unter 3 kaufen.
        assert_eq!(w.set_auto_trade(1, Good::Bread, 5, 5).unwrap_err().key, "error.auto_trade_range");
        w.set_auto_trade(1, Good::Bread, 3, 5).unwrap();
        assert_eq!(w.stock_state(1).auto_trade.len(), 1);
        for _ in 0..120 {
            w.step(0.05);
        }
        let bread = |w: &World| w.stock_state(1).goods.iter().find(|g| g.good == Good::Bread).map_or(0, |g| g.amount);
        assert_eq!(bread(&w), 5, "Überschuss verkauft");
        let all = bread(&w);
        w.trade(1, Good::Bread, all, true).unwrap();
        for _ in 0..120 {
            w.step(0.05);
        }
        assert_eq!(bread(&w), 3, "bis zur Untergrenze gekauft");
        w.set_auto_trade(1, Good::Bread, 0, 0).unwrap();
        assert!(w.stock_state(1).auto_trade.is_empty());
    }

    #[test]
    fn preise_wandern_mit_dem_handel_und_erholen_sich() {
        let row = "0".repeat(70);
        let text = format!("spawn 1 1\nspawn 65 65\n{}", format!("{row}\n").repeat(70));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        {
            let m = &mut w.economy.def.market;
            m.price_step_percent = 1.0;
            m.price_recovery_percent = 50.0;
            m.price_min_percent = 30.0;
            m.price_max_percent = 300.0;
            m.prices.retain(|p| p.good != Good::Wood);
            m.prices.push(protocol::Price { good: Good::Wood, buy: 100, sell: 80 });
        }
        w.spawn_player(1);
        w.spawn_player(2);
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }, GoodAmount { good: Good::Wood, amount: 300 }], 100_000);
        w.place_building(1, BuildingKind::Market, 5, 5).unwrap();
        w.place_building(1, BuildingKind::Storehouse, 12, 5).unwrap();
        let taler = w.stock_state(1).taler;
        // 10 verkaufen: jedes Stück etwas billiger, zusammen weniger als 10 × 80.
        w.trade(1, Good::Wood, 10, true).unwrap();
        let got = w.stock_state(1).taler - taler;
        assert!(got < 800 && got > 700, "Erlös {got}");
        let after_sell = w.price_now(Good::Wood).unwrap();
        assert!(after_sell.buy < 100 && after_sell.sell < 80, "{after_sell:?}");
        // Der andere Spieler sieht denselben (gesunkenen) Preis.
        let _ = w.take_dirty_stocks();
        w.trade(1, Good::Wood, 40, true).unwrap();
        let dirty = w.take_dirty_stocks();
        let p2 = dirty.iter().find(|(p, _)| *p == 2).expect("neue Preise gehen an alle").1.prices.iter().find(|p| p.good == Good::Wood).copied().unwrap();
        assert_eq!(p2, w.price_now(Good::Wood).unwrap());
        // Nie unter die Untergrenze.
        w.admin_give(1, &[GoodAmount { good: Good::Wood, amount: 500 }], 0);
        let _ = w.trade(1, Good::Wood, 400, true);
        assert!(w.price_now(Good::Wood).unwrap().buy >= 30);
        // Mit der Zeit wieder Richtung Grundpreis.
        let low = w.price_now(Good::Wood).unwrap().buy;
        for _ in 0..(20 * 120) {
            w.step(0.05);
        }
        let back = w.price_now(Good::Wood).unwrap().buy;
        assert!(back > low && back <= 100, "{low} → {back}");
        // Kaufen hebt den Preis.
        let wood = w.stock_state(1).goods.iter().find(|g| g.good == Good::Wood).map_or(0, |g| g.amount);
        w.economy.def.market.price_step_percent = 0.0;
        w.trade(1, Good::Wood, wood, true).unwrap();
        w.economy.def.market.price_step_percent = 1.0;
        let before = w.price_now(Good::Wood).unwrap().buy;
        w.trade(1, Good::Wood, 30, false).unwrap();
        assert!(w.price_now(Good::Wood).unwrap().buy > before);
    }
}
