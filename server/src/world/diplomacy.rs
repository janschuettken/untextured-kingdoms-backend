//! Bündnisse und Handel unter Verbündeten.
//!
//! Die Bündnisse legt die Lobby vor dem Start fest (`team` je Startplatz, gleiche
//! Zahl ab 1 = verbündet). Verbündete
//! - greifen sich nicht an (`can_attack`, für den Kampf, sobald es ihn gibt),
//! - gehen durch die Tore des anderen (automatische Tore schließen nicht vor ihnen),
//! - laufen auf seinen Mauern, Türmen und Toren (Wehrgang, Treppen, Besatzung),
//! - schicken sich Waren und Taler ohne Markt (`send_goods`).
//!
//! Wie die Waren ankommen, stellt der Gastgeber ein (`TradeMode`): sofort, oder
//! Handelskarren fahren vom eigenen Lager zum Lager des Verbündeten und
//! verschwinden dort. Was dort keinen Platz mehr hat, geht an den Absender zurück.

use protocol::{BuildingState, Good, GoodAmount, ServerMsg, Text, TradeMode, UnitKind};
use serde::{Deserialize, Serialize};

use super::{Refusal, Unit, World};

/// Sekunden, bis ein festgefahrener Handelskarren seinen Weg neu sucht.
const RETRY_SECONDS: f32 = 2.0;

/// Ziel eines Handelskarrens: Lager-Id, Tür, Weg dorthin.
type Route = (u32, (f32, f32), Vec<(f32, f32)>);

/// Ladung eines Handelskarrens.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delivery {
    /// Empfänger.
    pub to: u32,
    /// Lager des Empfängers, zu dem der Karren fährt.
    pub hub: u32,
    pub goods: Option<GoodAmount>,
    pub taler: u32,
}

impl World {
    /// Bündnis eines Spielers (0 = keins).
    pub fn team_of(&self, player: u32) -> u8 {
        self.teams.get(&player).map_or(0, |&t| t.min(u8::MAX as u32) as u8)
    }

    /// Bündnis eines Spielers setzen (0 = keins). Gilt auch für den Kampf (`combat.rs`).
    pub fn set_team(&mut self, player: u32, team: u8) {
        if team == 0 {
            self.teams.remove(&player);
        } else {
            self.teams.insert(player, team as u32);
        }
    }

    pub fn trade_mode(&self) -> TradeMode {
        self.trade_mode
    }

    pub fn set_trade_mode(&mut self, mode: TradeMode) {
        self.trade_mode = mode;
    }

    /// Derselbe Spieler oder im selben Bündnis.
    pub fn allied(&self, a: u32, b: u32) -> bool {
        a == b || self.teams.get(&a).is_some_and(|t| self.teams.get(&b) == Some(t))
    }

    /// Darf `attacker` Einheiten oder Gebäude von `target` angreifen? Nie sich selbst
    /// oder Verbündete.
    pub fn can_attack(&self, attacker: u32, target: u32) -> bool {
        !self.allied(attacker, target)
    }

    /// Waren und Taler an einen Verbündeten schicken.
    pub fn send_goods(&mut self, from: u32, to: u32, goods: &[GoodAmount], taler: u32) -> Result<(), Refusal> {
        if from == to || !self.has_player(to) {
            return Err(Text::new("error.trade_no_partner"));
        }
        if !self.allied(from, to) {
            return Err(Text::new("error.trade_not_allied"));
        }
        // Gleiche Waren zusammenfassen, leere weglassen.
        let mut list: Vec<GoodAmount> = Vec::new();
        for g in goods.iter().filter(|g| g.amount > 0) {
            match list.iter_mut().find(|l| l.good == g.good) {
                Some(l) => l.amount = l.amount.saturating_add(g.amount),
                None => list.push(*g),
            }
        }
        if list.is_empty() && taler == 0 {
            return Ok(());
        }
        let stock = self.stocks.get(&from).ok_or_else(|| Text::new("error.no_player"))?;
        for g in &list {
            let have = stock.amount(g.good);
            if have < g.amount {
                return Err(Text::new("error.not_enough").arg("good", good_key(g.good)).arg("need", g.amount - have));
            }
        }
        if stock.taler < taler {
            return Err(Text::new("error.not_enough_taler").arg("need", taler - stock.taler));
        }
        match self.trade_mode {
            TradeMode::Instant => self.send_instant(from, to, list, taler),
            TradeMode::Carts => self.send_carts(from, to, list, taler),
        }
    }

    /// Sofort: nur so viel, wie beim Empfänger Platz hat.
    fn send_instant(&mut self, from: u32, to: u32, list: Vec<GoodAmount>, taler: u32) -> Result<(), Refusal> {
        let room = self.stocks.get(&to).ok_or_else(|| Text::new("error.no_player"))?;
        let list: Vec<GoodAmount> = list
            .into_iter()
            .map(|g| GoodAmount { good: g.good, amount: g.amount.min(room.room(g.good)) })
            .filter(|g| g.amount > 0)
            .collect();
        if list.is_empty() && taler == 0 {
            return Err(Text::new("error.trade_partner_full"));
        }
        let src = self.stocks.get_mut(&from).expect("geprüft");
        src.take_all(&list);
        src.taler -= taler;
        let dst = self.stocks.get_mut(&to).expect("geprüft");
        for g in &list {
            dst.add(g.good, g.amount);
        }
        dst.taler = dst.taler.saturating_add(taler);
        self.dirty_stocks.insert(from);
        self.dirty_stocks.insert(to);
        self.events.push(ServerMsg::Shipment { from, to, goods: list, taler, arrived: true, carts: 0 });
        Ok(())
    }

    /// Mit Karren: je volle Karrenladung einer Ware ein Karren, Taler fahren mit dem ersten.
    fn send_carts(&mut self, from: u32, to: u32, list: Vec<GoodAmount>, taler: u32) -> Result<(), Refusal> {
        let cap = self.economy.def.transport.cart_capacity.max(1);
        let mut loads: Vec<(Option<GoodAmount>, u32)> = Vec::new();
        for g in &list {
            let mut left = g.amount;
            while left > 0 {
                let n = left.min(cap);
                loads.push((Some(GoodAmount { good: g.good, amount: n }), 0));
                left -= n;
            }
        }
        match loads.first_mut() {
            Some(first) => first.1 = taler,
            None => loads.push((None, taler)),
        }
        // Erst alle Wege planen, dann bezahlen: ganz oder gar nicht.
        let mut carts = Vec::new();
        for (good, money) in loads {
            let key = good.map(|g| g.good);
            let start = self.trade_start(from, key).ok_or_else(|| Text::new("error.trade_no_storage"))?;
            let (hub, door, path) = self.trade_route(to, start, key).ok_or_else(|| Text::new("error.trade_unreachable"))?;
            carts.push((start, hub, door, path, good, money));
        }
        let src = self.stocks.get_mut(&from).expect("geprüft");
        src.take_all(&list);
        src.taler -= taler;
        self.dirty_stocks.insert(from);
        let n = carts.len() as u32;
        for (start, hub, door, path, good, money) in carts {
            let id = self.next_unit_id;
            self.next_unit_id += 1;
            let mut u = Unit::new(id, from, UnitKind::TradeCart, start);
            u.path = path;
            u.target = Some(door);
            u.carrying = good;
            u.trade = Some(Delivery { to, hub, goods: good, taler: money });
            self.units.push(u);
        }
        self.events.push(ServerMsg::Shipment { from, to, goods: list, taler, arrived: false, carts: n });
        Ok(())
    }

    /// Wo ein Handelskarren des Absenders losfährt: am Lager, in dem die Ware liegt.
    fn trade_start(&self, from: u32, good: Option<Good>) -> Option<(f32, f32)> {
        let keep = self.buildings.iter().find(|b| b.owner == from && b.kind == protocol::BuildingKind::Keep);
        let near = keep.map_or((0.0, 0.0), center);
        let hub = match good {
            Some(g) => self.nearest_hub_having(from, near, g).and_then(|id| self.buildings.iter().find(|b| b.id == id)),
            None => None,
        };
        let b = hub.or(keep).or_else(|| self.hubs_of(from).next())?;
        self.door(b, near)
    }

    fn hubs_of(&self, owner: u32) -> impl Iterator<Item = &BuildingState> {
        self.buildings.iter().filter(move |b| b.owner == owner && self.economy.building(b.kind).is_some_and(|d| d.hub))
    }

    /// Lager des Empfängers, seine Tür und der Weg dorthin (nur wenn er ankommt).
    fn trade_route(&self, to: u32, from: (f32, f32), good: Option<Good>) -> Option<Route> {
        let preferred = good.and_then(|g| self.nearest_hub(to, from, g));
        let mut hubs: Vec<&BuildingState> = self.hubs_of(to).collect();
        hubs.sort_by(|a, b| {
            let rank = |h: &BuildingState| (Some(h.id) != preferred, dist2(center(h), from));
            let (ra, rb) = (rank(a), rank(b));
            ra.0.cmp(&rb.0).then(ra.1.total_cmp(&rb.1))
        });
        hubs.into_iter().take(3).find_map(|h| {
            let door = self.door(h, from)?;
            let path = self.map.nav.waypoints(from, door);
            let end = path.last().copied().unwrap_or(from);
            self.at_door(h, end).then_some((h.id, door, path))
        })
    }

    /// Handelskarren: angekommen abladen und verschwinden, festgefahren neu suchen.
    pub(super) fn step_trade(&mut self, dt: f32) {
        let mut arrived = Vec::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            let Some(d) = &u.trade else { continue };
            if !u.path.is_empty() {
                continue;
            }
            let (pos, to, hub_id, good) = (u.pos, d.to, d.hub, d.goods.map(|g| g.good));
            let hub = self.buildings.iter().find(|b| b.id == hub_id && b.owner == to);
            if hub.is_some_and(|h| self.at_door(h, pos)) || !self.has_player(to) {
                arrived.push(k);
                continue;
            }
            // Ziel weg oder Weg versperrt (z. B. Tor zu): ab und zu neu versuchen.
            let u = &mut self.units[k];
            u.cooldown -= dt;
            if u.cooldown > 0.0 {
                continue;
            }
            u.cooldown = RETRY_SECONDS;
            if let Some((hub, door, path)) = self.trade_route(to, pos, good) {
                let u = &mut self.units[k];
                u.path = path;
                u.target = Some(door);
                if let Some(d) = &mut u.trade {
                    d.hub = hub;
                }
            }
        }
        for k in arrived.into_iter().rev() {
            let u = self.units.remove(k);
            let Some(d) = u.trade else { continue };
            self.deliver(u.owner, d);
        }
    }

    /// Ladung beim Empfänger einlagern; was nicht passt, geht an den Absender zurück.
    fn deliver(&mut self, from: u32, d: Delivery) {
        let mut back = None;
        let mut got = Vec::new();
        if let Some(dst) = self.stocks.get_mut(&d.to) {
            if let Some(g) = d.goods {
                let n = dst.add(g.good, g.amount);
                if n > 0 {
                    got.push(GoodAmount { good: g.good, amount: n });
                }
                if n < g.amount {
                    back = Some(GoodAmount { good: g.good, amount: g.amount - n });
                }
            }
            dst.taler = dst.taler.saturating_add(d.taler);
            self.dirty_stocks.insert(d.to);
        } else {
            back = d.goods;
        }
        if let (Some(g), Some(src)) = (back, self.stocks.get_mut(&from)) {
            src.add_unlimited(g.good, g.amount);
            self.dirty_stocks.insert(from);
        }
        if !got.is_empty() || d.taler > 0 {
            self.events.push(ServerMsg::Shipment { from, to: d.to, goods: got, taler: d.taler, arrived: true, carts: 1 });
        }
    }
}

fn center(b: &BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

fn good_key(good: Good) -> String {
    serde_json::to_value(good).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount, TradeMode, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    /// Zwei Burgen auf flacher Karte, Spieler 1 und 2.
    fn two() -> World {
        let row = "0".repeat(60);
        let text = format!("spawn 8 8\nspawn 50 50\n{}", format!("{row}\n").repeat(60));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player_at(1, (8, 8));
        w.spawn_player_at(2, (50, 50));
        assert!(w.buildings.iter().filter(|b| b.kind == BuildingKind::Keep).count() == 2);
        w.admin_give(1, &[GoodAmount { good: Good::Stone, amount: 100 }], 100);
        w
    }

    fn amount(w: &World, p: u32, g: Good) -> u32 {
        w.stocks[&p].amount(g)
    }

    #[test]
    fn nur_unter_verbuendeten() {
        let mut w = two();
        let stone = [GoodAmount { good: Good::Stone, amount: 10 }];
        assert_eq!(w.send_goods(1, 2, &stone, 0).unwrap_err().key, "error.trade_not_allied");
        assert!(w.can_attack(1, 2));
        w.set_team(1, 3);
        w.set_team(2, 3);
        assert!(w.allied(1, 2) && !w.can_attack(1, 2) && !w.can_attack(2, 1));
        assert!(!w.can_attack(1, 1));
        assert_eq!(w.send_goods(1, 9, &stone, 0).unwrap_err().key, "error.trade_no_partner");
        let too_much = [GoodAmount { good: Good::Stone, amount: 1_000_000 }];
        assert_eq!(w.send_goods(1, 2, &too_much, 0).unwrap_err().key, "error.not_enough");
    }

    #[test]
    fn sofort_beim_verbuendeten() {
        let mut w = two();
        w.set_team(1, 1);
        w.set_team(2, 1);
        let (s1, s2) = (amount(&w, 1, Good::Stone), amount(&w, 2, Good::Stone));
        let (t1, t2) = (w.stocks[&1].taler, w.stocks[&2].taler);
        w.send_goods(1, 2, &[GoodAmount { good: Good::Stone, amount: 10 }], 5).unwrap();
        assert_eq!(amount(&w, 1, Good::Stone), s1 - 10);
        assert_eq!(amount(&w, 2, Good::Stone), s2 + 10);
        assert_eq!((w.stocks[&1].taler, w.stocks[&2].taler), (t1 - 5, t2 + 5));
        assert!(!w.units.iter().any(|u| u.kind == UnitKind::TradeCart));
    }

    #[test]
    fn karren_fahren_hin_und_verschwinden() {
        let mut w = two();
        w.set_team(1, 1);
        w.set_team(2, 1);
        w.set_trade_mode(TradeMode::Carts);
        let cap = w.economy.def.transport.cart_capacity;
        let (s1, s2) = (amount(&w, 1, Good::Stone), amount(&w, 2, Good::Stone));
        let n = cap + 1; // zwei Karren
        w.send_goods(1, 2, &[GoodAmount { good: Good::Stone, amount: n }], 7).unwrap();
        assert_eq!(amount(&w, 1, Good::Stone), s1 - n, "beim Losfahren abgezogen");
        assert_eq!(amount(&w, 2, Good::Stone), s2, "noch nicht da");
        assert_eq!(w.units.iter().filter(|u| u.kind == UnitKind::TradeCart).count(), 2);
        // Befehle gelten nicht für Handelskarren.
        let cart = w.units.iter().find(|u| u.kind == UnitKind::TradeCart).unwrap().id;
        assert!(!w.order_move(1, &[cart], (1.5, 1.5)));
        for _ in 0..20 * 120 {
            w.step(0.05);
            if !w.units.iter().any(|u| u.kind == UnitKind::TradeCart) {
                break;
            }
        }
        assert!(!w.units.iter().any(|u| u.kind == UnitKind::TradeCart), "Karren sind angekommen und weg");
        assert_eq!(amount(&w, 2, Good::Stone), s2 + n);
        // Spielstand mit fahrenden Karren lässt sich speichern und laden.
        w.send_goods(1, 2, &[GoodAmount { good: Good::Stone, amount: 3 }], 0).unwrap();
        let json = serde_json::to_string(&w.to_save("t")).unwrap();
        let row = "0".repeat(60);
        let text = format!("spawn 8 8\nspawn 50 50\n{}", format!("{row}\n").repeat(60));
        let l = World::from_save(GameMap::parse(&text).unwrap(), w.economy.clone(), serde_json::from_str(&json).unwrap()).unwrap();
        assert!(l.allied(1, 2));
        assert_eq!(l.trade_mode(), TradeMode::Carts);
        assert!(l.units.iter().any(|u| u.kind == UnitKind::TradeCart && u.trade.is_some()));
    }
}
