//! Fortschritt des laufenden Durchlaufs je Betrieb, für den Balken im Client.
//!
//! Der Server schickt Stand (0..1) und Tempo (Anteil je Spielsekunde); der
//! Client rechnet selbst weiter. Neu geschickt wird nur, wenn die Rechnung des
//! Clients merklich danebenläge (neuer Durchlauf, Pause, Hunger, voll …).

use std::collections::HashMap;

use protocol::{BuildingStatus, UnitKind};

use super::{gather::Gather, World};

/// So weit darf die Schätzung des Clients abweichen, bevor neu geschickt wird.
const TOLERANCE: f32 = 0.03;

impl World {
    pub(super) fn step_progress(&mut self, dt: f32) {
        // Abbaubetriebe: der am weitesten fortgeschrittene Arbeiter am Feld.
        let mut gathering: HashMap<u32, (f32, f32)> = HashMap::new();
        for u in &self.units {
            let (UnitKind::Worker, Some(home), Some(Gather::Working { left, .. })) = (u.kind, u.home, u.gather) else { continue };
            let Some(b) = self.buildings.iter().find(|b| b.id == home) else { continue };
            let Some(recipe) = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)) else { continue };
            let seconds = recipe.seconds.max(0.01);
            let p = (1.0 - left / seconds).clamp(0.0, 1.0);
            let rate = self.hunger_factor(b.owner) / seconds;
            let e = gathering.entry(home).or_insert((0.0, 0.0));
            if p >= e.0 {
                *e = (p, rate);
            }
        }
        for i in 0..self.buildings.len() {
            let b = &self.buildings[i];
            let id = b.id;
            let gathers = self.economy.building(b.kind).is_some_and(|d| !d.gathers.is_empty());
            let training = b.training.first().and_then(|&k| self.economy.unit(k)).map(|u| u.seconds.max(0.01));
            let (p, rate) = if let Some(seconds) = training {
                // Kaserne: Ausbildung der vordersten Einheit in der Warteschlange.
                let left = b.training_left.max(0.0);
                ((1.0 - left / seconds).clamp(0.0, 1.0), if left > 0.0 { 1.0 / seconds } else { 0.0 })
            } else if gathers {
                gathering.get(&id).copied().unwrap_or((0.0, 0.0))
            } else {
                let recipe = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r));
                match (recipe, self.work.get(&id).and_then(|w| w.elapsed)) {
                    (Some(r), Some(e)) => {
                        let seconds = r.seconds.max(0.01);
                        let running = b.status == BuildingStatus::Working;
                        let rate = if running { self.speed(i) * self.hunger_factor(b.owner) / seconds } else { 0.0 };
                        ((e / seconds).min(1.0), rate)
                    }
                    _ => (0.0, 0.0),
                }
            };
            let clock = self.progress_clock.entry(id).or_insert(0.0);
            *clock += dt;
            let b = &mut self.buildings[i];
            let guess = (b.progress + b.progress_rate * *clock).min(1.0);
            let rate_changed = (rate - b.progress_rate).abs() > 0.05 * rate.max(b.progress_rate) + 1e-6;
            if (p - guess).abs() > TOLERANCE || rate_changed {
                b.progress = p;
                b.progress_rate = rate;
                b.progress_seq = b.progress_seq.wrapping_add(1);
                *clock = 0.0;
                self.dirty_buildings.insert(id);
            }
        }
    }
}
