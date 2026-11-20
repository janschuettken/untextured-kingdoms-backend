//! Brücken über Flüsse.
//!
//! Eine Brücke ist ein Gebäude über einer geraden Reihe Wasserfelder (eine Seite 1 Feld,
//! höchstens `max_span` lang, an beiden Enden Ufer). Ihre Felder werden im Laufgitter
//! begehbar wie Boden; der Belag steigt gleichmäßig vom einen Ufer zum anderen.
//! Brücken aus der Kartendatei gehören niemandem (Besitzer [`NEUTRAL`]): sie lassen sich
//! nur mit „Alles angreifen“ einreißen. Fällt eine Brücke, stürzen alle darauf ins Wasser.

use protocol::{BuildingKind, BuildingSettings, BuildingState, BuildingStatus, GateMode, GoodAmount, ServerMsg, Text, UnitKind};

use super::{Refusal, World};

/// Besitzer neutraler Objekte (aus dem Karteneditor oder Kartengenerator).
pub const NEUTRAL: u32 = 0;

/// Ohne Wert in economy.json: so lang darf eine Brücke werden.
const DEFAULT_SPAN: u32 = crate::map::MAX_BRIDGE_SPAN;
/// Ohne Brücke in economy.json: Lebenspunkte neutraler Brücken.
const DEFAULT_HP: u32 = 400;

/// Länge der Brücke in Feldern.
pub fn bridge_len(b: &BuildingState) -> u32 {
    b.w.max(b.h)
}

impl World {
    /// Längste erlaubte Brücke (economy.json `max_span`, sonst 15).
    pub fn max_bridge_span(&self) -> u32 {
        match self.economy.building(BuildingKind::Bridge).map_or(0, |d| d.max_span) {
            0 => DEFAULT_SPAN,
            n => n.min(64),
        }
    }

    /// Was eine Brücke dieser Länge kostet: die Kosten der Stufe 1 je Feld.
    pub fn bridge_cost(&self, len: u32) -> Vec<GoodAmount> {
        self.economy
            .building(BuildingKind::Bridge)
            .map(|d| super::level_cost(d, 1).into_iter().map(|c| GoodAmount { good: c.good, amount: c.amount * len }).collect())
            .unwrap_or_default()
    }

    /// Prüft, ob der Spieler hier eine Brücke bauen dürfte (ohne Kosten).
    pub fn check_bridge(&self, player: u32, rect: (u32, u32, u32, u32)) -> Result<Vec<((i32, i32), u8)>, Refusal> {
        let def = self.economy.building(BuildingKind::Bridge).ok_or_else(|| Text::new("error.unknown_building"))?;
        let decks = self.map.bridge_decks(rect, self.max_bridge_span())?;
        let (x, y, w, h) = rect;
        for &((cx, cy), _) in &decks {
            if self.occupied[(cy as u32 * self.map.width + cx as u32) as usize] != 0 {
                return Err(Text::new("error.occupied"));
            }
        }
        // Beide Ufer müssen begehbar sein (kein Gebäude, kein Wald davor).
        let along_x = self.map.bridge_along_x(rect);
        let len = w.max(h) as i32;
        let (dx, dy) = if along_x { (1, 0) } else { (0, 1) };
        let banks = [(x as i32 - dx, y as i32 - dy), (x as i32 + dx * len, y as i32 + dy * len)];
        if banks.iter().any(|&(bx, by)| !self.map.nav.is_open(bx, by) && !self.map.nav.has_deck(bx, by)) {
            return Err(Text::new("error.bridge_no_bank"));
        }
        self.check_build_radius(player, def.at(1).build_radius, (x, y), (w, h))?;
        self.check_no_enemy(player, def.kind, (x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0))?;
        Ok(decks)
    }

    /// Brücke bauen (Befehl `place_bridge`). Kostet die Stufe-1-Kosten je Feld.
    pub fn place_bridge(&mut self, player: u32, rect: (u32, u32, u32, u32)) -> Result<BuildingState, Refusal> {
        self.check_bridge(player, rect)?;
        let cost = self.bridge_cost(rect.2.max(rect.3));
        self.pay(player, &cost)?;
        Ok(self.add_bridge(player, rect))
    }

    /// Neutrale Brücken aus der Karte anlegen (beim Spielstart).
    pub(super) fn add_map_bridges(&mut self) {
        for rect in self.map.bridges.clone() {
            self.add_bridge(NEUTRAL, rect);
        }
    }

    fn add_bridge(&mut self, owner: u32, rect: (u32, u32, u32, u32)) -> BuildingState {
        let (x, y, w, h) = rect;
        let hp = self.economy.building(BuildingKind::Bridge).map_or(DEFAULT_HP, |d| d.at(1).max_hp.max(1));
        let b = BuildingState {
            id: self.next_building_id,
            owner,
            kind: BuildingKind::Bridge,
            x,
            y,
            w,
            h,
            hp,
            level: 1,
            settings: BuildingSettings {
                recipe: None,
                paused: false,
                accept_deliveries: false,
                stores: None,
                keep_in_stock: Vec::new(),
                gate: GateMode::Auto,
                worker_carries: false,
                cart_deliveries: false,
                stop_at: Vec::new(),
            },
            status: BuildingStatus::Idle,
            rotation: if self.map.bridge_along_x(rect) { 0 } else { 1 },
            entrance: None,
            inventory: Vec::new(),
            gate_open: false,
            training: Vec::new(),
            training_left: 0.0,
            progress: 0.0,
            progress_rate: 0.0,
            progress_seq: 0,
            road_under: 0,
            capacity: 0,
            mature_in: 0.0,
        };
        self.next_building_id += 1;
        self.set_footprint(&b, true);
        self.buildings.push(b.clone());
        self.replan_all();
        b
    }

    /// Felder der Brücke belegen und im Laufgitter begehbar machen (oder wieder Wasser).
    pub(super) fn set_bridge_footprint(&mut self, b: &BuildingState, on: bool) {
        let rect = (b.x, b.y, b.w, b.h);
        let decks = match self.map.bridge_decks(rect, u32::MAX) {
            Ok(d) => d,
            Err(_) => return,
        };
        for &((cx, cy), _) in &decks {
            self.occupied[(cy as u32 * self.map.width + cx as u32) as usize] = if on { b.id } else { 0 };
        }
        // Liegen noch andere Brücken auf denselben Feldern (sollte nicht vorkommen), bleibt der Belag.
        let cells: Vec<((i32, i32), Option<u8>)> = decks.into_iter().map(|(c, l)| (c, on.then_some(l))).collect();
        self.map.nav.set_deck(&cells);
    }

    /// Die Brücke fällt: wer darauf steht, stürzt ins Wasser und ertrinkt.
    pub(super) fn drown_on_bridge(&mut self, b: &BuildingState) {
        let on = |p: (f32, f32)| {
            let (tx, ty) = (p.0.floor() as i64, p.1.floor() as i64);
            tx >= b.x as i64 && ty >= b.y as i64 && tx < (b.x + b.w) as i64 && ty < (b.y + b.h) as i64
        };
        let mut died = Vec::new();
        let mut kings = Vec::new();
        let mut owners = Vec::new();
        self.units.retain(|u| {
            if !on(u.pos) || u.on_wall {
                return true;
            }
            died.push(u.id);
            owners.push(u.owner);
            if u.kind == UnitKind::King {
                kings.push(u.owner);
            }
            false
        });
        if died.is_empty() {
            return;
        }
        for o in owners {
            self.dirty_stocks.insert(o);
        }
        self.events.push(ServerMsg::UnitsDied { unit_ids: died });
        let any_king = !kings.is_empty();
        for k in kings {
            self.knock_out(k, None);
        }
        if any_king {
            self.check_victory();
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{AttackTarget, BuildingKind, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    /// 20 × 9 Felder, quer ein Fluss (Zeilen 3..6, also 3 Felder breit). Spieler 1 oben.
    fn river(extra: &str) -> World {
        let land = "1".repeat(20);
        let water = "~".repeat(20);
        let text = format!("spawn 2 1\nspawn 2 7\n{extra}[terrain]\n{land}\n{land}\n{land}\n{water}\n{water}\n{water}\n{land}\n{land}\n{land}\n");
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people().without_build_radius();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.admin_give(1, &[GoodAmount { good: Good::Wood, amount: 500 }, GoodAmount { good: Good::Boards, amount: 500 }], 0);
        w
    }

    fn reachable(w: &World, from: (i32, i32), to: (i32, i32)) -> bool {
        w.map.nav.find_path(from, to).last() == Some(&to)
    }

    #[test]
    fn ohne_bruecke_kein_weg_mit_bruecke_schon() {
        let mut w = river("");
        assert!(!reachable(&w, (5, 1), (5, 7)));
        let b = w.place_bridge(1, (10, 3, 1, 3)).expect("Brücke passt");
        assert_eq!(b.owner, 1);
        assert!(reachable(&w, (5, 1), (5, 7)), "über die Brücke");
        w.demolish(1, b.id).unwrap();
        assert!(!reachable(&w, (5, 1), (5, 7)), "abgerissen: wieder Wasser");
    }

    #[test]
    fn bruecke_braucht_wasser_ufer_und_hoechstens_15_felder() {
        let mut w = river("");
        assert_eq!(w.place_bridge(1, (10, 2, 1, 4)).unwrap_err().key, "error.bridge_needs_water");
        assert_eq!(w.place_bridge(1, (10, 3, 1, 2)).unwrap_err().key, "error.bridge_no_bank");
        assert_eq!(w.place_bridge(1, (10, 3, 2, 3)).unwrap_err().key, "error.bridge_straight");
        assert_eq!(w.place_bridge(1, (0, 3, 16, 1)).unwrap_err().key, "error.bridge_too_long");
    }

    #[test]
    fn kosten_je_feld() {
        let mut w = river("");
        let per = w.bridge_cost(1);
        let before: Vec<u32> = per.iter().map(|c| w.stocks[&1].amount(c.good)).collect();
        w.place_bridge(1, (10, 3, 1, 3)).unwrap();
        for (c, b) in per.iter().zip(before) {
            assert_eq!(w.stocks[&1].amount(c.good), b - c.amount * 3);
        }
    }

    #[test]
    fn karten_bruecke_ist_neutral_und_geht_nur_mit_zwang() {
        let mut w = river("bridge 10 3 1 3\n");
        let b = w.buildings.iter().find(|b| b.kind == BuildingKind::Bridge).cloned().expect("aus der Karte");
        assert_eq!(b.owner, super::NEUTRAL);
        assert!(reachable(&w, (5, 1), (5, 7)));
        assert!(w.demolish(1, b.id).is_err(), "gehört niemandem");
        let king = w.units.iter().find(|u| u.owner == 1 && u.kind == UnitKind::King).unwrap().id;
        let id = w.next_unit_id;
        w.next_unit_id += 1;
        let mut s = crate::world::Unit::new(id, 1, UnitKind::Swordsman, (10.5, 2.5));
        s.home = None;
        w.units.push(s);
        // Normaler Angriff auf Neutrales: nur hinlaufen.
        w.order_attack(1, &[id], AttackTarget::Building { id: b.id });
        assert!(w.units.iter().find(|u| u.id == id).unwrap().attack_order().is_none());
        // Mit Zwang: einreißen.
        w.order_attack_full(1, &[id], AttackTarget::Building { id: b.id }, true);
        for _ in 0..20 * 120 {
            w.step(0.05);
            if !w.buildings.iter().any(|o| o.id == b.id) {
                break;
            }
        }
        assert!(!w.buildings.iter().any(|o| o.id == b.id), "Brücke eingerissen");
        assert!(!reachable(&w, (5, 1), (5, 7)));
        assert!(w.units.iter().any(|u| u.id == king), "König stand nicht drauf");
    }

    #[test]
    fn wer_auf_der_bruecke_steht_ertrinkt() {
        let mut w = river("");
        let b = w.place_bridge(1, (10, 3, 1, 3)).unwrap();
        let id = w.next_unit_id;
        w.next_unit_id += 1;
        w.units.push(crate::world::Unit::new(id, 1, UnitKind::Swordsman, (10.5, 4.5)));
        w.demolish(1, b.id).unwrap();
        assert!(!w.units.iter().any(|u| u.id == id));
    }

    #[test]
    fn spielstand_behaelt_bruecken() {
        let mut w = river("bridge 10 3 1 3\n");
        w.place_bridge(1, (14, 3, 1, 3)).unwrap();
        let save = w.to_save("t");
        let land = "1".repeat(20);
        let water = "~".repeat(20);
        let text = format!("spawn 2 1\nspawn 2 7\nbridge 10 3 1 3\n[terrain]\n{land}\n{land}\n{land}\n{water}\n{water}\n{water}\n{land}\n{land}\n{land}\n");
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let l = World::from_save(GameMap::parse(&text).unwrap(), eco, save).unwrap();
        assert_eq!(l.buildings.iter().filter(|b| b.kind == BuildingKind::Bridge).count(), 2);
        assert!(l.map.nav.find_path((15, 1), (15, 7)).last() == Some(&(15, 7)));
    }
}
