//! Gedränge auflösen: Soldaten (und König, Heiler), die stehen, rücken auseinander,
//! wenn sie einander zu nah sind. So stapeln sich nach Kämpfen, beim Ausbilden oder
//! bei mehreren Befehlen auf denselben Fleck keine Einheiten übereinander.
//!
//! Abstand und Tempo stehen in `movement.spread_distance` und `movement.spread_speed`
//! (0 schaltet es ab). Nachbarn findet ein Raster (Spatial Hash), damit es auch mit
//! sehr vielen Einheiten schnell bleibt. Geschoben wird nur auf begehbare Felder:
//! am Boden nur über erlaubte Schritte (kein Wald, kein Wasser, keine Klippe),
//! oben nur auf dem Wehrgang desselben Bauwerks (nicht auf Turmplattformen).

use std::collections::HashMap;

use super::{Unit, World};

/// Reiter brauchen mehr Platz als Fußvolk.
const MOUNTED_SIZE: f32 = 1.5;
/// Etwas über den Mindestabstand hinaus schieben, damit danach Ruhe ist (kein Zittern).
const SLACK: f32 = 0.02;

impl Unit {
    /// Nimmt am Auseinanderrücken teil (als Schieber oder Geschobener).
    fn crowds(&self) -> bool {
        !self.is_civilian() && !self.kind.is_siege()
    }

    /// Steht still und darf geschoben werden. Wer läuft, schiebt nur.
    fn pushable(&self) -> bool {
        self.path.is_empty() && self.down <= 0.0
    }

    fn radius(&self, base: f32) -> f32 {
        base / 2.0 * if self.mounted { MOUNTED_SIZE } else { 1.0 }
    }
}

impl World {
    pub(super) fn step_spread(&mut self, dt: f32) {
        let mv = &self.economy.def.movement;
        let (gap, speed) = (mv.spread_distance, mv.spread_speed);
        if !(gap > 0.0 && speed > 0.0 && dt > 0.0) {
            return;
        }
        let cell_size = gap * MOUNTED_SIZE;
        let key = |p: (f32, f32), wall: bool| ((p.0 / cell_size).floor() as i32, (p.1 / cell_size).floor() as i32, wall);
        let mut grid: HashMap<(i32, i32, bool), Vec<usize>> = HashMap::new();
        let mut any_still = false;
        for (k, u) in self.units.iter().enumerate() {
            if u.crowds() {
                grid.entry(key(u.pos, u.on_wall)).or_default().push(k);
                any_still |= u.pushable();
            }
        }
        if !any_still {
            return;
        }
        let max_step = speed * dt;
        let mut moves: Vec<(usize, (f32, f32))> = Vec::new();
        for (k, u) in self.units.iter().enumerate() {
            if !u.crowds() || !u.pushable() {
                continue;
            }
            let (cx, cy, wall) = key(u.pos, u.on_wall);
            let mut push = (0.0f32, 0.0f32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let Some(list) = grid.get(&(cx + dx, cy + dy, wall)) else { continue };
                    for &j in list {
                        if j == k {
                            continue;
                        }
                        let o = &self.units[j];
                        let min = u.radius(gap) + o.radius(gap);
                        let (ex, ey) = (u.pos.0 - o.pos.0, u.pos.1 - o.pos.1);
                        let d = ex.hypot(ey);
                        if d >= min {
                            continue;
                        }
                        // Genau auf demselben Punkt: feste Richtung aus den beiden Nummern,
                        // die beiden gehen in entgegengesetzte Richtungen.
                        let dir = if d > 1e-4 {
                            (ex / d, ey / d)
                        } else {
                            let (a, b) = (u.id.min(o.id), u.id.max(o.id));
                            let angle = (a.wrapping_mul(2_654_435_761) ^ b.wrapping_mul(40_503)) as f32 * 1e-3;
                            let s = if u.id < o.id { 1.0 } else { -1.0 };
                            (angle.cos() * s, angle.sin() * s)
                        };
                        // Stehen beide, geht jeder die Hälfte; ein Laufender weicht nicht aus.
                        let share = if o.pushable() { 0.5 } else { 1.0 };
                        let amount = (min + SLACK - d) * share;
                        push.0 += dir.0 * amount;
                        push.1 += dir.1 * amount;
                    }
                }
            }
            let len = push.0.hypot(push.1);
            if len < 1e-4 {
                continue;
            }
            let f = len.min(max_step) / len;
            let step = (push.0 * f, push.1 * f);
            // Notfalls nur entlang einer Achse (an Wald oder Mauer entlang gleiten).
            let tries = [step, (step.0, 0.0), (0.0, step.1)];
            if let Some(to) = tries.iter().map(|s| (u.pos.0 + s.0, u.pos.1 + s.1)).find(|&p| self.may_shift(u, p)) {
                moves.push((k, to));
            }
        }
        for (k, to) in moves {
            let u = &mut self.units[k];
            u.pos = to;
            // Das Ziel wandert mit, sonst läuft die Einheit nach einer neuen Wegsuche zurück.
            if u.target.is_some() {
                u.target = Some(to);
            }
        }
    }

    /// Darf die stehende Einheit ein kleines Stück nach `to` rücken?
    fn may_shift(&self, u: &Unit, to: (f32, f32)) -> bool {
        let (w, h) = (self.map.width as f32, self.map.height as f32);
        if !(to.0 >= 0.0 && to.1 >= 0.0 && to.0 < w && to.1 < h) {
            return false;
        }
        let from = (u.pos.0.floor() as i32, u.pos.1.floor() as i32);
        let cell = (to.0.floor() as i32, to.1.floor() as i32);
        if u.on_wall {
            let (Some(a), Some(b)) = (self.tile_of(u.pos), self.tile_of(to)) else { return false };
            let on = self.occupied[a];
            // Türme mit begrenzter Besatzung: die Plätze auf der kleinen Plattform verteilt
            // schon der Laufbefehl, dort wird nicht geschoben.
            return on == self.occupied[b] && self.platform(on).is_none() && self.is_walkway(u.owner, to);
        }
        let nav = &self.map.nav;
        if cell == from {
            return true;
        }
        if !nav.is_open(cell.0, cell.1) {
            return false;
        }
        let (dx, dy) = (cell.0 - from.0, cell.1 - from.1);
        // Stand die Einheit auf einem gesperrten Feld, darf sie auf jedes freie daneben.
        !nav.is_open(from.0, from.1) || (dx.abs() <= 1 && dy.abs() <= 1 && nav.can_step(from.0, from.1, dx, dy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::Economy;
    use crate::map::GameMap;
    use protocol::UnitKind;

    fn world() -> World {
        let row = "0".repeat(40);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(40));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people().without_build_radius();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.units.clear();
        w
    }

    fn steps(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as u32 {
            w.step(0.05);
        }
    }

    fn min_gap(w: &World) -> f32 {
        let mut best = f32::MAX;
        for (i, a) in w.units.iter().enumerate() {
            for b in &w.units[i + 1..] {
                best = best.min((a.pos.0 - b.pos.0).hypot(a.pos.1 - b.pos.1));
            }
        }
        best
    }

    #[test]
    fn viele_einheiten_auf_einem_fleck_ruecken_auseinander() {
        let mut w = world();
        let gap = w.economy.def.movement.spread_distance;
        assert!(gap > 0.0);
        for i in 0..30 {
            w.units.push(Unit::new(50_000 + i, 1, UnitKind::Spearman, (20.5, 20.5)));
        }
        steps(&mut w, 20.0);
        assert!(min_gap(&w) >= gap * 0.9, "Abstand {} statt {gap}", min_gap(&w));
        // Sie bleiben in der Nähe (verteilen sich, laufen nicht weg).
        for u in &w.units {
            assert!((u.pos.0 - 20.5).hypot(u.pos.1 - 20.5) < 3.0, "zu weit weg: {:?}", u.pos);
        }
        // Danach ist Ruhe: niemand rückt mehr.
        let before: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        steps(&mut w, 1.0);
        let after: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        assert_eq!(before, after, "kein Zittern, wenn alle Platz haben");
    }

    #[test]
    fn auseinanderruecken_nur_auf_begehbare_felder() {
        let mut w = world();
        // Fast alles rundherum gesperrt (wie Wald): nur das Feld selbst und eins rechts davon.
        for y in 15..26 {
            for x in 15..26 {
                if (x, y) != (20, 20) && (x, y) != (21, 20) {
                    w.map.nav.set_blocked(x, y, true);
                }
            }
        }
        for i in 0..12 {
            w.units.push(Unit::new(51_000 + i, 1, UnitKind::Archer, (20.5, 20.5)));
        }
        steps(&mut w, 10.0);
        for u in &w.units {
            let cell = (u.pos.0.floor() as i32, u.pos.1.floor() as i32);
            assert!(cell == (20, 20) || cell == (21, 20), "auf gesperrtem Feld: {:?}", u.pos);
        }
        assert!(w.units.iter().any(|u| u.pos.0 >= 21.0), "einige weichen aufs freie Nachbarfeld aus");
    }

    #[test]
    fn laufende_einheiten_werden_nicht_geschoben_und_verteilte_bleiben_stehen() {
        let mut w = world();
        for i in 0..6 {
            w.units.push(Unit::new(52_000 + i, 1, UnitKind::Swordsman, (10.5, 10.5)));
        }
        steps(&mut w, 10.0);
        // Neue Wegsuche (z. B. ein Gebäude entsteht) holt niemanden auf den alten Fleck zurück.
        let spread: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        let ids: Vec<u32> = w.units.iter().map(|u| u.id).collect();
        assert!(w.order_move(1, &ids, (30.5, 30.5)));
        w.replan_all();
        assert!(w.units.iter().all(|u| !u.path.is_empty()), "alle laufen");
        let start: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        assert_eq!(spread, start);
        steps(&mut w, 30.0);
        assert!(min_gap(&w) >= w.economy.def.movement.spread_distance * 0.9, "am Ziel verteilt: {}", min_gap(&w));
        let before: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        w.replan_all();
        steps(&mut w, 2.0);
        let after: Vec<_> = w.units.iter().map(|u| u.pos).collect();
        assert_eq!(before, after, "nach neuer Wegsuche bleiben alle stehen");
    }

    #[test]
    fn abstand_null_schaltet_es_ab() {
        let mut w = world();
        let mut eco = w.economy.clone();
        eco.def.movement.spread_distance = 0.0;
        w.set_economy(eco);
        for i in 0..4 {
            w.units.push(Unit::new(53_000 + i, 1, UnitKind::Spearman, (5.5, 5.5)));
        }
        steps(&mut w, 5.0);
        assert!(w.units.iter().all(|u| u.pos == (5.5, 5.5)));
    }
}
