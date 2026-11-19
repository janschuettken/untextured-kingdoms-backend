//! Lagerbestand eines Spielers.
//!
//! Jede Ware liegt in Chargen (Batches). Heute verdirbt nichts, aber jede
//! Charge hat schon ein optionales Verfallsdatum: Für verderbliche Waren
//! (Brot, Fleisch, ...) bekommt eine neue Charge später `expires_at` und
//! `expire(now)` wirft abgelaufene Chargen weg. Entnommen wird immer die
//! älteste Charge zuerst, so verdirbt am wenigsten.

use std::collections::{BTreeMap, VecDeque};

use protocol::{BuildingKind, Good, GoodAmount, StockState, StoragePool};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Batch {
    amount: u32,
    /// Spieltakt, ab dem die Charge verdorben ist. `None` = hält ewig.
    expires_at: Option<u64>,
}

/// Lagerplatz einer Lagerart: Gesamtplatz, den sich alle Waren teilen, die sie nimmt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pool {
    pub kind: BuildingKind,
    /// Waren, die hier hinein dürfen; `None` = alle.
    pub goods: Option<Vec<Good>>,
    pub capacity: u32,
}

impl Pool {
    fn accepts(&self, good: Good) -> bool {
        self.goods.as_ref().is_none_or(|g| g.contains(&good))
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Stock {
    goods: BTreeMap<Good, VecDeque<Batch>>,
    /// Höchstmenge je Ware (Betriebe).
    pub capacity: u32,
    /// Vorrat eines Spielers: Gesamtplatz je Lagerart statt `capacity` je Ware.
    #[serde(default)]
    pub pooled: bool,
    #[serde(default)]
    pub pools: Vec<Pool>,
    /// Feste Höchstmenge für einzelne Waren, statt `capacity` (Betriebe: Eingänge).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<Good, u32>,
    /// Geld, ohne Obergrenze.
    pub taler: u32,
}

impl Stock {
    pub fn new(capacity: u32) -> Stock {
        Stock { goods: BTreeMap::new(), capacity, pooled: false, pools: Vec::new(), limits: BTreeMap::new(), taler: 0 }
    }

    /// Vorrat eines Spielers: Platz kommt aus den Lagern (`pools`).
    pub fn pooled() -> Stock {
        Stock { pooled: true, ..Stock::new(0) }
    }

    /// Belegung je Lagerart: Waren mit wenigen passenden Lagern zuerst, jeweils
    /// ins engste passende Lager (Kornspeicher vor Vorratslager).
    fn allocation(&self) -> Vec<u32> {
        let mut used = vec![0u32; self.pools.len()];
        let width = |p: &Pool| p.goods.as_ref().map_or(usize::MAX, |g| g.len());
        let mut order: Vec<usize> = (0..self.pools.len()).collect();
        order.sort_by_key(|&i| width(&self.pools[i]));
        let mut goods: Vec<(Good, u32)> = self.goods.keys().map(|&g| (g, self.amount(g))).filter(|g| g.1 > 0).collect();
        goods.sort_by_key(|&(g, _)| self.pools.iter().filter(|p| p.accepts(g)).count());
        for (good, mut left) in goods {
            for &i in order.iter().filter(|&&i| self.pools[i].accepts(good)) {
                let n = self.pools[i].capacity.saturating_sub(used[i]).min(left);
                used[i] += n;
                left -= n;
            }
        }
        used
    }

    /// Höchstmenge dieser Ware.
    pub fn capacity_of(&self, good: Good) -> u32 {
        if let Some(&n) = self.limits.get(&good) {
            return n;
        }
        if self.pooled {
            return self.amount(good) + self.room(good);
        }
        self.capacity
    }

    /// So viel passt von dieser Ware noch hinein.
    pub fn room(&self, good: Good) -> u32 {
        if self.pooled && !self.limits.contains_key(&good) {
            let used = self.allocation();
            return self.pools.iter().zip(used).filter(|(p, _)| p.accepts(good)).map(|(p, u)| p.capacity.saturating_sub(u)).sum();
        }
        self.capacity_of(good).saturating_sub(self.amount(good))
    }

    pub fn amount(&self, good: Good) -> u32 {
        self.goods.get(&good).map_or(0, |b| b.iter().map(|b| b.amount).sum())
    }

    /// Ware einlagern, höchstens bis zur Kapazität. Liefert, was wirklich hineinpasst.
    pub fn add(&mut self, good: Good, amount: u32) -> u32 {
        let fits = amount.min(self.room(good));
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
        // Je Lagerart zusammengefasst (gleiche Art mit unterschiedlicher Auswahl zählt zusammen).
        let mut pools: Vec<StoragePool> = Vec::new();
        for (p, used) in self.pools.iter().zip(self.allocation()) {
            match pools.iter_mut().find(|q| q.kind == p.kind) {
                Some(q) => {
                    q.capacity += p.capacity;
                    q.used += used;
                }
                None => pools.push(StoragePool { kind: p.kind, capacity: p.capacity, used, goods: p.goods.clone().unwrap_or_default() }),
            }
        }
        let capacity = if self.pooled { self.pools.iter().map(|p| p.capacity).sum() } else { self.capacity };
        StockState { goods, capacity, pools, people: Default::default(), taler: self.taler, army: Default::default(), auto_trade: Vec::new() }
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
    fn gesamtplatz_je_lager() {
        let mut s = Stock::pooled();
        let food = vec![Good::Bread, Good::Meat];
        s.pools = vec![
            Pool { kind: BuildingKind::Storehouse, goods: None, capacity: 100 },
            Pool { kind: BuildingKind::Granary, goods: Some(food), capacity: 50 },
        ];
        // Beliebige Mischung bis zum Gesamtplatz.
        assert_eq!(s.add(Good::Wood, 60), 60);
        assert_eq!(s.add(Good::Stone, 60), 40, "Vorratslager voll: 100 zusammen");
        assert_eq!(s.room(Good::Wood), 0);
        // Essen geht zuerst in den Kornspeicher, Rest ins Vorratslager (voll).
        assert_eq!(s.add(Good::Bread, 70), 50);
        assert_eq!(s.room(Good::Meat), 0);
        assert!(s.take_all(&[GoodAmount { good: Good::Wood, amount: 30 }]));
        assert_eq!(s.room(Good::Meat), 30, "Platz im Vorratslager wieder frei");
        let st = s.state();
        assert_eq!(st.capacity, 150);
        assert_eq!(st.pools.iter().map(|p| p.used).sum::<u32>(), 120);
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
