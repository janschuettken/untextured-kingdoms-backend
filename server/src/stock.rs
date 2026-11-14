//! Lagerbestand eines Spielers.
//!
//! Jede Ware liegt in Chargen (Batches). Heute verdirbt nichts, aber jede
//! Charge hat schon ein optionales Verfallsdatum: Für verderbliche Waren
//! (Brot, Fleisch, ...) bekommt eine neue Charge später `expires_at` und
//! `expire(now)` wirft abgelaufene Chargen weg. Entnommen wird immer die
//! älteste Charge zuerst, so verdirbt am wenigsten.

use std::collections::{BTreeMap, VecDeque};

use protocol::{Good, GoodAmount, StockState};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Batch {
    amount: u32,
    /// Spieltakt, ab dem die Charge verdorben ist. `None` = hält ewig.
    expires_at: Option<u64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Stock {
    goods: BTreeMap<Good, VecDeque<Batch>>,
    /// Höchstmenge je Ware.
    pub capacity: u32,
    /// Geld, ohne Obergrenze.
    pub taler: u32,
}

impl Stock {
    pub fn new(capacity: u32) -> Stock {
        Stock { goods: BTreeMap::new(), capacity, taler: 0 }
    }

    pub fn amount(&self, good: Good) -> u32 {
        self.goods.get(&good).map_or(0, |b| b.iter().map(|b| b.amount).sum())
    }

    /// Ware einlagern, höchstens bis zur Kapazität. Liefert, was wirklich hineinpasst.
    pub fn add(&mut self, good: Good, amount: u32) -> u32 {
        let fits = amount.min(self.capacity.saturating_sub(self.amount(good)));
        if fits > 0 {
            self.goods.entry(good).or_default().push_back(Batch { amount: fits, expires_at: None });
        }
        fits
    }

    /// Ware einlagern, auch über die Kapazität hinaus (Startgüter, Admin).
    pub fn add_unlimited(&mut self, good: Good, amount: u32) {
        if amount > 0 {
            self.goods.entry(good).or_default().push_back(Batch { amount, expires_at: None });
        }
    }

    pub fn has_all(&self, cost: &[GoodAmount]) -> bool {
        cost.iter().all(|c| self.amount(c.good) >= c.amount)
    }

    /// Kosten abziehen, alles oder nichts.
    pub fn take_all(&mut self, cost: &[GoodAmount]) -> bool {
        if !self.has_all(cost) {
            return false;
        }
        for c in cost {
            let mut left = c.amount;
            let batches = self.goods.entry(c.good).or_default();
            while left > 0 {
                let front = batches.front_mut().expect("Menge geprüft");
                let n = front.amount.min(left);
                front.amount -= n;
                left -= n;
                if front.amount == 0 {
                    batches.pop_front();
                }
            }
        }
        true
    }

    /// Verdorbene Chargen entfernen (noch ohne Wirkung, da nichts verdirbt).
    #[allow(dead_code)]
    pub fn expire(&mut self, now: u64) {
        for batches in self.goods.values_mut() {
            batches.retain(|b| b.expires_at.is_none_or(|t| t > now));
        }
    }

    pub fn state(&self) -> StockState {
        let goods = self
            .goods
            .keys()
            .map(|&good| GoodAmount { good, amount: self.amount(good) })
            .filter(|g| g.amount > 0)
            .collect();
        StockState { goods, capacity: self.capacity, people: Default::default(), taler: self.taler, army: Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lager_rechnet_richtig() {
        let mut s = Stock::new(100);
        assert_eq!(s.add(Good::Wood, 80), 80);
        assert_eq!(s.add(Good::Wood, 50), 20, "nur bis zur Kapazität");
        let cost = [GoodAmount { good: Good::Wood, amount: 90 }, GoodAmount { good: Good::Stone, amount: 1 }];
        assert!(!s.take_all(&cost), "Stein fehlt");
        assert_eq!(s.amount(Good::Wood), 100, "nichts abgezogen");
        assert!(s.take_all(&cost[..1]));
        assert_eq!(s.amount(Good::Wood), 10);
        assert_eq!(s.state().goods, vec![GoodAmount { good: Good::Wood, amount: 10 }]);
    }

    #[test]
    fn abgelaufene_chargen_verschwinden() {
        let mut s = Stock::new(100);
        s.goods.entry(Good::Bread).or_default().push_back(Batch { amount: 5, expires_at: Some(10) });
        s.add(Good::Bread, 3);
        s.expire(10);
        assert_eq!(s.amount(Good::Bread), 3);
    }
}
