//! Markt: Waren gegen Taler kaufen und verkaufen.
//!
//! Vorerst ein gemeinsamer Markt für alle (Preise aus `market.prices`, fest).
//! Handeln darf, wer einen eigenen Markt mit Arbeiter hat; die Ware geht sofort
//! in den Vorrat bzw. aus ihm heraus. Taler kommen sonst aus Steuern
//! (`market.tax_per_meal` je satter Bewohner und Mahlzeit, siehe `people.rs`).

use protocol::{BuildingKind, Good, GoodAmount, Text};

use super::{Refusal, World};

impl World {
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
            if stock.amount(good) < amount {
                return Err(Text::new("error.not_enough").arg("good", good_key(good)).arg("need", amount - stock.amount(good)));
            }
            stock.take_all(&[GoodAmount { good, amount }]);
            stock.taler = stock.taler.saturating_add(price.sell * amount);
        } else {
            let room = stock.capacity.saturating_sub(stock.amount(good));
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
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        let price = *w.economy.def.market.prices.iter().find(|p| p.good == Good::Bread).unwrap();
        assert_eq!(w.trade(1, Good::Bread, 1, true).unwrap_err().key, "error.no_market");
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }, GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::Market, 5, 5).unwrap();
        let taler = w.stock_state(1).taler;
        w.trade(1, Good::Bread, 10, true).unwrap();
        assert_eq!(w.stock_state(1).taler, taler + 10 * price.sell);
        w.trade(1, Good::Bread, 2, false).unwrap();
        assert_eq!(w.stock_state(1).taler, taler + 10 * price.sell - 2 * price.buy);
        assert_eq!(w.trade(1, Good::Crossbow, 1000, false).unwrap_err().key, "error.not_enough_taler");
        assert_eq!(w.trade(1, Good::Bread, 1000, true).unwrap_err().key, "error.not_enough");
    }
}
