//! Zeitstrahl eines Spiels: wer wann wen angegriffen hat (gleiche Vorfälle wie die
//! Warnungen in `alerts.rs`, aber ohne Sperre und für Angreifer und Opfer), Meilensteine
//! (erster Soldat, erste mittlere/schwere Einheit, erstes Belagerungsgerät, Burg geschlossen),
//! Ausscheiden und Spielende. Steht im Spielstand.
//!
//! Angriffe sind Vorfälle: Kommt der nächste Treffer desselben Angreifers auf denselben
//! Spieler (gleiche Art) innerhalb von `INCIDENT_GAP` Sekunden, wird der Eintrag verlängert.

use std::collections::HashMap;

use protocol::{BuildingKind, Milestone, TimelineEntry, TimelineEvent, UnitKind};

use super::World;

/// Sekunden ohne neuen Treffer, nach denen ein Angriff als vorbei gilt.
const INCIDENT_GAP: f64 = 30.0;
/// So viele Einträge höchstens (die ältesten Angriffe fallen dann heraus).
const MAX_ENTRIES: usize = 4000;

/// Laufende Vorfälle: (Art, Opfer, Angreifer) → Index im Zeitstrahl. Nicht im Spielstand;
/// nach dem Laden beginnt ein neuer Eintrag.
#[derive(Debug, Default)]
pub(crate) struct Incidents(HashMap<(u8, u32, u32), usize>);

impl World {
    fn push_timeline(&mut self, event: TimelineEvent) -> usize {
        if self.timeline.len() >= MAX_ENTRIES {
            // Den ältesten Angriff wegwerfen; Meilensteine und Ausscheiden bleiben.
            if let Some(i) = self.timeline.iter().position(|e| is_attack(&e.event)) {
                self.timeline.remove(i);
                self.incidents.0.retain(|_, k| *k != i);
                for k in self.incidents.0.values_mut() {
                    if *k > i {
                        *k -= 1;
                    }
                }
            }
        }
        let at = self.game_seconds;
        self.timeline.push(TimelineEntry { at, until: at, event });
        self.timeline.len() - 1
    }

    /// Angriff eintragen oder den laufenden Vorfall verlängern.
    fn note_attack(&mut self, kind: u8, player: u32, by: u32, building: Option<BuildingKind>) {
        if player == super::NEUTRAL || by == super::NEUTRAL || player == by {
            return;
        }
        let now = self.game_seconds;
        let key = (kind, player, by);
        if let Some(&i) = self.incidents.0.get(&key) {
            if let Some(e) = self.timeline.get_mut(i).filter(|e| now - e.until <= INCIDENT_GAP) {
                e.until = now;
                if let (TimelineEvent::BuildingAttacked { buildings, .. }, Some(b)) = (&mut e.event, building) {
                    if !buildings.contains(&b) {
                        buildings.push(b);
                    }
                }
                return;
            }
        }
        let event = match kind {
            0 => TimelineEvent::EnemyNear { player, by },
            1 => TimelineEvent::BuildingAttacked { player, by, buildings: building.into_iter().collect() },
            _ => TimelineEvent::UnitsAttacked { player, by },
        };
        let i = self.push_timeline(event);
        self.incidents.0.insert(key, i);
    }

    /// Kämpfer von `by` sind nahe der Burg von `player` (aus `alerts.rs`, einmal je Sekunde).
    pub(crate) fn timeline_enemy_near(&mut self, player: u32, by: u32) {
        self.note_attack(0, player, by, None);
    }

    /// `by` hat ein Gebäude von `player` beschädigt.
    pub(crate) fn timeline_building_attacked(&mut self, player: u32, by: u32, building: BuildingKind) {
        self.note_attack(1, player, by, Some(building));
    }

    /// `by` hat eine Einheit von `player` getroffen.
    pub(crate) fn timeline_units_attacked(&mut self, player: u32, by: u32) {
        self.note_attack(2, player, by, None);
    }

    /// Neue Einheit (Kaserne oder fertiges Belagerungsgerät): Meilensteine prüfen.
    pub(crate) fn timeline_unit_ready(&mut self, player: u32, unit: UnitKind) {
        for &m in unit.milestones() {
            self.timeline_milestone(player, m, Some(unit));
        }
    }

    /// Meilenstein eintragen, wenn der Spieler ihn noch nicht hat.
    pub(crate) fn timeline_milestone(&mut self, player: u32, milestone: Milestone, unit: Option<UnitKind>) {
        if player == super::NEUTRAL || self.has_milestone(player, milestone) {
            return;
        }
        self.push_timeline(TimelineEvent::Milestone { player, milestone, unit });
    }

    pub(crate) fn has_milestone(&self, player: u32, milestone: Milestone) -> bool {
        self.timeline
            .iter()
            .any(|e| matches!(e.event, TimelineEvent::Milestone { player: p, milestone: m, .. } if p == player && m == milestone))
    }

    pub(crate) fn timeline_defeated(&mut self, player: u32, by: Option<u32>) {
        self.push_timeline(TimelineEvent::PlayerDefeated { player, by });
    }

    pub(crate) fn timeline_game_over(&mut self, winners: Vec<u32>) {
        self.push_timeline(TimelineEvent::GameOver { winners });
    }

    /// Was `viewer` sehen darf: solange das Spiel läuft und er dabei ist, nur Einträge,
    /// die ihn oder seine Verbündeten betreffen (Ausscheiden und Spielende sieht jeder).
    pub fn timeline_for(&self, viewer: u32) -> Vec<TimelineEntry> {
        if self.winners.is_some() || self.is_eliminated(viewer) {
            return self.timeline.clone();
        }
        let mine = |p: u32| self.allied(viewer, p);
        self.timeline
            .iter()
            .filter(|e| match &e.event {
                TimelineEvent::EnemyNear { player, by }
                | TimelineEvent::BuildingAttacked { player, by, .. }
                | TimelineEvent::UnitsAttacked { player, by } => mine(*player) || mine(*by),
                TimelineEvent::Milestone { player, .. } => mine(*player),
                TimelineEvent::PlayerDefeated { .. } | TimelineEvent::GameOver { .. } => true,
            })
            .cloned()
            .collect()
    }
}

fn is_attack(e: &TimelineEvent) -> bool {
    matches!(e, TimelineEvent::EnemyNear { .. } | TimelineEvent::BuildingAttacked { .. } | TimelineEvent::UnitsAttacked { .. })
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Milestone, TimelineEvent, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn world() -> World {
        let row = "0".repeat(60);
        let text = format!("spawn 1 1\nspawn 58 58\nspawn 1 58\n{}", format!("{row}\n").repeat(60));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.spawn_player(2);
        w.spawn_player(3);
        w
    }

    #[test]
    fn angriffe_werden_zu_vorfaellen_gebuendelt() {
        let mut w = world();
        w.timeline_building_attacked(1, 2, BuildingKind::Keep);
        w.game_seconds = 10.0;
        w.timeline_building_attacked(1, 2, BuildingKind::Woodcutter);
        w.game_seconds = 25.0;
        w.timeline_building_attacked(1, 2, BuildingKind::Keep);
        assert_eq!(w.timeline.len(), 1);
        assert_eq!(w.timeline[0].until, 25.0);
        assert!(matches!(&w.timeline[0].event, TimelineEvent::BuildingAttacked { buildings, .. } if buildings == &vec![BuildingKind::Keep, BuildingKind::Woodcutter]));
        // Lange Ruhe: neuer Vorfall. Anderer Angreifer: eigener Eintrag.
        w.game_seconds = 100.0;
        w.timeline_building_attacked(1, 2, BuildingKind::Keep);
        w.timeline_building_attacked(1, 3, BuildingKind::Keep);
        w.timeline_units_attacked(1, 2);
        assert_eq!(w.timeline.len(), 4);
    }

    #[test]
    fn meilensteine_nur_einmal() {
        let mut w = world();
        w.timeline_unit_ready(1, UnitKind::Spearman);
        w.timeline_unit_ready(1, UnitKind::Swordsman);
        w.timeline_unit_ready(1, UnitKind::Crossbowman);
        w.timeline_unit_ready(1, UnitKind::Worker);
        let got: Vec<Milestone> = w
            .timeline
            .iter()
            .filter_map(|e| match e.event {
                TimelineEvent::Milestone { milestone, .. } => Some(milestone),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![Milestone::FirstSoldier, Milestone::FirstMedium]);
    }

    #[test]
    fn gegner_sehen_fremde_eintraege_erst_am_ende() {
        let mut w = world();
        w.timeline_unit_ready(2, UnitKind::Knight);
        w.timeline_units_attacked(3, 2);
        w.timeline_units_attacked(1, 2);
        // Spieler 1 sieht nur den Angriff auf sich.
        assert_eq!(w.timeline_for(1).len(), 1);
        assert_eq!(w.timeline_for(2).len(), 4);
        w.eliminate(3, Some(2));
        w.eliminate(1, Some(2));
        // Spiel entschieden: alles für alle.
        assert_eq!(w.timeline_for(1).len(), w.timeline.len());
        assert!(w.timeline.iter().any(|e| matches!(&e.event, TimelineEvent::GameOver { winners } if winners == &vec![2])));
    }
}
