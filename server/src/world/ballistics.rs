//! Fernkampf: Kommt der Pfeil beim Ziel an, oder steht etwas im Weg?
//!
//! Gerechnet wird in Weltmaß wie im Client: ein Feld breit = 1, eine Geländestufe = 0,5.
//! Gebäude ragen `height` über den Boden (siehe `BuildingDef::height`).
//!
//! Zwei Regeln, die der Gastgeber in der Lobby wählt (`ShotMode`):
//! - Wurfbahn: Abschuss immer mit dem Tempo, das auf ebenem Boden genau die Reichweite
//!   schafft. Bis zu zwei Bahnen treffen (flach und steil); stoßen beide an Gelände,
//!   Gebäude oder Mauer, schießt der Schütze nicht. Wer höher steht, kommt weiter.
//! - Sichtlinie: gerade Linie vom Auge zum Ziel, sie muss frei sein.

use protocol::{BuildingState, ShotMode};

use super::{Unit, World};

/// Weltmaß einer Geländestufe (wie `LEVEL_HEIGHT` im Client).
pub(super) const LEVEL_HEIGHT: f32 = 0.5;
/// Abschuss- und Trefferhöhe über den Füßen.
const BODY: f32 = 0.7;
/// Auf dem Wehrgang steht man so weit unter der Oberkante (Zinnen).
const BATTLEMENT: f32 = 0.35;
/// Schrittweite beim Abtasten der Bahn (Felder).
const STEP: f32 = 0.2;
/// So weit um Schütze und Ziel wird nichts geprüft (der eigene Körper, die Mauer, an der man steht).
const CLEAR: f32 = 0.6;

/// Wie der Schuss fliegt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flight {
    Straight,
    /// Wurfbahn mit dieser Abschuss-Steigung (tan).
    Arc(f32),
}

impl World {
    pub fn shot_mode(&self) -> ShotMode {
        self.shot_mode
    }

    pub fn set_shot_mode(&mut self, mode: ShotMode) {
        self.shot_mode = mode;
    }

    fn ground_h(&self, x: f32, y: f32) -> f32 {
        self.map.height_at(x, y) * LEVEL_HEIGHT
    }

    /// Wie hoch ragt dieses Gebäude über den Boden?
    pub(super) fn building_height(&self, b: &BuildingState) -> f32 {
        let Some(def) = self.economy.building(b.kind) else { return 0.0 };
        let storeys = if def.category == protocol::BuildingCategory::Fortification { 0.0 } else { b.level.saturating_sub(1) as f32 * 0.6 };
        def.height + storeys
    }

    /// Gebäude auf dem Feld unter diesem Punkt.
    fn building_at(&self, x: f32, y: f32) -> Option<&BuildingState> {
        if x < 0.0 || y < 0.0 || x >= self.map.width as f32 || y >= self.map.height as f32 {
            return None;
        }
        let id = self.occupied[(y as u32 * self.map.width + x as u32) as usize];
        if id == 0 {
            return None;
        }
        self.buildings.iter().find(|b| b.id == id)
    }

    /// Höhe der Füße einer Einheit (Boden oder Wehrgang).
    pub(super) fn feet_h(&self, u: &Unit) -> f32 {
        let ground = self.ground_h(u.pos.0, u.pos.1);
        if u.on_wall {
            if let Some(b) = self.building_at(u.pos.0, u.pos.1) {
                let top = self.ground_h(b.x as f32 + 0.5, b.y as f32 + 0.5) + self.building_height(b) - BATTLEMENT;
                return top.max(ground);
            }
        }
        ground
    }

    /// Bis wohin ist an dieser Stelle alles fest (Gelände und Gebäude)? `ignore`: diese Gebäude nicht.
    fn solid_top(&self, x: f32, y: f32, ignore: &[u32]) -> f32 {
        let ground = self.ground_h(x, y);
        match self.building_at(x, y) {
            Some(b) if !ignore.contains(&b.id) => {
                let base = self.ground_h(b.x as f32 + 0.5, b.y as f32 + 0.5).max(ground);
                base + self.building_height(b)
            }
            _ => ground,
        }
    }

    /// Kann `from` mit Reichweite `range` das Ziel `to` treffen? `None` = etwas im Weg (oder zu hoch).
    pub(super) fn flight(&self, from: &Unit, to: &Unit, range: f32) -> Option<Flight> {
        let (h0, h1) = (self.feet_h(from) + BODY, self.feet_h(to) + BODY);
        // Wer auf einer Mauer steht, schießt über sie hinweg und wird über sie hinweg getroffen.
        let mut ignore = Vec::new();
        for u in [from, to] {
            if u.on_wall {
                if let Some(b) = self.building_at(u.pos.0, u.pos.1) {
                    ignore.push(b.id);
                }
            }
        }
        let (dx, dy) = (to.pos.0 - from.pos.0, to.pos.1 - from.pos.1);
        let d = dx.hypot(dy);
        if d < 1e-3 {
            return Some(Flight::Straight);
        }
        let clear = |height: &dyn Fn(f32) -> f32| {
            let n = (d / STEP).ceil() as u32;
            (1..n).all(|i| {
                let s = d * i as f32 / n as f32;
                if s < CLEAR || d - s < CLEAR {
                    return true;
                }
                let (x, y) = (from.pos.0 + dx * s / d, from.pos.1 + dy * s / d);
                height(s) > self.solid_top(x, y, &ignore)
            })
        };
        match self.shot_mode {
            ShotMode::LineOfSight => clear(&|s| h0 + (h1 - h0) * s / d).then_some(Flight::Straight),
            ShotMode::Ballistic => {
                // Wurfbahn mit v² = g · Reichweite: h(s) = h0 + s·T − s²(1 + T²) / (2R).
                // Treffen bei s = d: a·T² − d·T + (dz + a) = 0 mit a = d² / (2R).
                let r = range.max(0.1);
                let a = d * d / (2.0 * r);
                let disc = d * d - 4.0 * a * (h1 - h0 + a);
                if disc < 0.0 {
                    return None; // zu weit für diesen Höhenunterschied
                }
                let root = disc.sqrt();
                [(d - root) / (2.0 * a), (d + root) / (2.0 * a)]
                    .into_iter()
                    .find(|&t| clear(&|s| h0 + s * t - s * s * (1.0 + t * t) / (2.0 * r)))
                    .map(Flight::Arc)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, ShotMode, UnitKind};

    use super::Flight;
    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    /// Flache Karte, ein Turm in der Mitte.
    fn world() -> World {
        let row = "0".repeat(40);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(20));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people().without_build_radius();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.admin_give(1, &[protocol::GoodAmount { good: protocol::Good::Stone, amount: 500 }, protocol::GoodAmount { good: protocol::Good::Wood, amount: 500 }, protocol::GoodAmount { good: protocol::Good::Boards, amount: 500 }, protocol::GoodAmount { good: protocol::Good::Blocks, amount: 500 }], 1000);
        w.place_building(1, BuildingKind::Tower, 20, 10).unwrap();
        w
    }

    fn unit(x: f32, y: f32) -> Unit {
        Unit::new(500, 2, UnitKind::Archer, (x, y))
    }

    #[test]
    fn wurfbahn_geht_ueber_den_turm_sichtlinie_nicht() {
        let mut w = world();
        // Rot links vom Turm, Gelb weit rechts davon, Blau direkt hinter dem Turm.
        let red = unit(14.5, 10.5);
        let yellow = unit(30.5, 10.5);
        let blue = unit(22.8, 10.5);
        assert!(matches!(w.flight(&red, &yellow, 18.0), Some(Flight::Arc(_))), "über den Turm zu Gelb");
        w.set_shot_mode(ShotMode::LineOfSight);
        assert_eq!(w.flight(&red, &yellow, 18.0), None, "Turm verdeckt Gelb");
        assert_eq!(w.flight(&red, &blue, 18.0), None, "Turm verdeckt Blau");
        let free = unit(14.5, 4.5);
        assert_eq!(w.flight(&red, &free, 18.0), Some(Flight::Straight), "freie Sicht daneben");
    }

    #[test]
    fn zu_weit_fuer_die_wurfbahn() {
        let w = world();
        let a = unit(1.5, 2.5);
        let b = unit(30.5, 2.5);
        assert_eq!(w.flight(&a, &b, 18.0), None);
        let c = unit(18.5, 2.5);
        assert!(w.flight(&a, &c, 18.0).is_some());
    }
}
