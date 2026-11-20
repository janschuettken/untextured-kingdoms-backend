//! Rechenzeit je Teilschritt eines Ticks (für die Serverauslastung).
//! Jeder Tick sammelt in einem `PhaseTimes`, wie lange welcher Teil gebraucht hat.

use std::time::{Duration, Instant};

/// Teilschritte eines Ticks, in der Reihenfolge, in der sie laufen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Phase {
    Ai,
    People,
    Production,
    Transport,
    Gather,
    Progress,
    Gates,
    Army,
    Horses,
    Trade,
    Combat,
    Waypoints,
    Heal,
    Alerts,
    Orchards,
    Food,
    Replan,
    Hubs,
    Movement,
    Spread,
    Buildings,
    Castle,
    Send,
    Save,
}

/// Namen für die Anzeige (Schlüssel für die Übersetzung im Client: `load.phase.<name>`).
pub const PHASE_NAMES: [&str; PHASE_COUNT] = [
    "ai",
    "people",
    "production",
    "transport",
    "gather",
    "progress",
    "gates",
    "army",
    "horses",
    "trade",
    "combat",
    "waypoints",
    "heal",
    "alerts",
    "orchards",
    "food",
    "replan",
    "hubs",
    "movement",
    "spread",
    "buildings",
    "castle",
    "send",
    "save",
];

pub const PHASE_COUNT: usize = Phase::Save as usize + 1;

/// Mikrosekunden je Teilschritt in einem Tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhaseTimes(pub [u32; PHASE_COUNT]);

impl PhaseTimes {
    pub fn add(&mut self, phase: Phase, took: Duration) {
        let us = took.as_micros().min(u32::MAX as u128) as u32;
        let slot = &mut self.0[phase as usize];
        *slot = slot.saturating_add(us);
    }

    pub fn merge(&mut self, other: &PhaseTimes) {
        for (a, b) in self.0.iter_mut().zip(other.0.iter()) {
            *a = a.saturating_add(*b);
        }
    }

    /// `f` ausführen und seine Zeit unter `phase` aufschreiben.
    pub fn time<R>(&mut self, phase: Phase, f: impl FnOnce() -> R) -> R {
        let t = Instant::now();
        let r = f();
        self.add(phase, t.elapsed());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namen_passen_zu_den_teilschritten() {
        assert_eq!(PHASE_NAMES[Phase::Ai as usize], "ai");
        assert_eq!(PHASE_NAMES[Phase::Movement as usize], "movement");
        assert_eq!(PHASE_NAMES[Phase::Save as usize], "save");
    }

    #[test]
    fn zeiten_addieren_sich() {
        let mut p = PhaseTimes::default();
        p.add(Phase::Combat, Duration::from_micros(30));
        p.add(Phase::Combat, Duration::from_micros(12));
        let mut q = PhaseTimes::default();
        q.add(Phase::Combat, Duration::from_micros(8));
        q.merge(&p);
        assert_eq!(q.0[Phase::Combat as usize], 50);
    }
}
