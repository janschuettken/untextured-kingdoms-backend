//! Markt: Waren gegen Taler kaufen und verkaufen.
//!
//! Vorerst ein gemeinsamer Markt für alle (Preise aus `market.prices`, fest).
//! Handeln darf, wer einen eigenen Markt mit Arbeiter hat; die Ware geht sofort
//! in den Vorrat bzw. aus ihm heraus. Taler kommen sonst aus Steuern
//! (`market.tax_per_meal` je satter Bewohner und Mahlzeit, siehe `people.rs`).

use protocol::{BuildingKind, Good, GoodAmount, Text};

use super::{Refusal, World};

/// Alle so viele Sekunden (Spielzeit) wird automatisch gehandelt.
const AUTO_TRADE_SECONDS: f32 = 5.0;

impl World {
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
        self.auto_trade_clock += dt;
        if self.auto_trade_clock < AUTO_TRADE_SECONDS {
            return;
        }
        self.auto_trade_clock = 0.0;
        let rules: Vec<(u32, protocol::AutoTrade)> = self.auto_trade.iter().flat_map(|(&p, r)| r.iter().map(move |r| (p, *r))).collect();
        for (player, r) in rules {
            if self.eliminated.contains(&player) {
                continue;
            }
            let Some(stock) = self.stocks.get(&player) else { continue };
            let have = stock.amount(r.good);
            let Some(price) = self.economy.def.market.prices.iter().find(|p| p.good == r.good).copied() else { continue };
            if r.sell_above > 0 && have > r.sell_above {
                let _ = self.trade(player, r.good, have - r.sell_above, true);
            } else if r.buy_below > 0 && have < r.buy_below {
                let afford = stock.taler.checked_div(price.buy).unwrap_or(u32::MAX);
                let amount = (r.buy_below - have).min(afford).min(stock.room(r.good));
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
        let price = self.economy.def.market.prices.iter().find(|p| p.good == good).copied().ok_or_else(|| Text::new("error.not_traded"))?;
        let stock = self.stocks.get_mut(&player).ok_or_else(|| Text::new("error.no_player"))?;
        if sell {
            // Verkaufspreis 0: die Ware nimmt der Markt nicht an (z. B. Heilsalbe).
            if price.sell == 0 {
                return Err(Text::new("error.not_sellable"));
            }
            if stock.amount(good) < amount {
                return Err(Text::new("error.not_enough").arg("good", good_key(good)).arg("need", amount - stock.amount(good)));
            }
            stock.take_all(&[GoodAmount { good, amount }]);
            stock.taler = stock.taler.saturating_add(price.sell * amount);
        } else {
            let room = stock.room(good);
            let amount = amount.min(room);
            if amount == 0 {
                return Err(Text::new("error.storage_full"));
            }
            let cost = price.buy * amount;
            if stock.taler < cost {
                return Err(Text::new("error.not_enough_taler").arg("need", cost - stock.taler));
            }
            stock.taler -= cost;
            stock.add(good, amount);
        }
        self.dirty_stocks.insert(player);
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
}
