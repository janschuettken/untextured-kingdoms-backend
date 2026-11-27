//! Warnungen an Spieler (`ServerMsg::Alert`): feindliche Kämpfer nahe der eigenen Burg
//! (Umkreis `army.alert_radius` um den Bergfried) und Angriffe auf eigene Gebäude.
//!
//! Damit nichts dauernd aufpoppt: Eine Warnung gilt für einen ganzen Vorfall. „Feinde nahe“
//! kommt erst wieder, wenn die Feinde weg waren und `army.alert_seconds` vergangen sind;
//! „Gebäude angegriffen“ höchstens alle `army.alert_seconds`, solange weiter Schaden kommt.

use std::collections::{HashMap, HashSet};

use protocol::{AlertKind, BuildingKind, ServerMsg};

use super::World;

/// Wie oft (Sekunden) nach Feinden um die Burg geschaut wird.
const CHECK_SECONDS: f32 = 1.0;

#[derive(Debug, Default)]
pub(crate) struct Alerts {
    /// Sekunden bis zum nächsten Blick auf Feinde nahe der Burg.
    clock: f32,
    /// Sperre je Spieler und Art: so viele Sekunden noch keine neue Warnung.
    cooldown: HashMap<(u32, AlertKind), f32>,
    /// Spieler, bei denen beim letzten Blick Feinde nahe der Burg waren.
    enemies_near: HashSet<u32>,
}

impl World {
    pub(crate) fn step_alerts(&mut self, dt: f32) {
        for left in self.alerts.cooldown.values_mut() {
            *left -= dt;
        }
        self.alerts.cooldown.retain(|_, left| *left > 0.0);
        self.alerts.clock -= dt;
        if self.alerts.clock > 0.0 {
            return;
        }
        self.alerts.clock = CHECK_SECONDS;
        let radius = self.economy.def.army.alert_radius;
        if radius <= 0.0 {
            self.alerts.enemies_near.clear();
            return;
        }
        let keeps: Vec<(u32, (f32, f32))> = self
            .buildings
            .iter()
            .filter(|b| b.kind == BuildingKind::Keep && !self.is_eliminated(b.owner))
            .map(|b| (b.owner, (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)))
            .collect();
        let mut near_now = HashSet::new();
        for (player, at) in keeps {
            if near_now.contains(&player) {
                continue;
            }
            // Der nächste feindliche Kämpfer im Umkreis (dorthin springt die Kamera).
            let nearest = self
                .units
                .iter()
                .filter(|u| self.economy.unit(u.kind).is_some_and(|d| d.attack > 0) && self.can_attack(player, u.owner))
                .map(|u| (u.pos, (u.pos.0 - at.0).hypot(u.pos.1 - at.1)))
                .filter(|&(_, d)| d <= radius)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let Some((pos, _)) = nearest else { continue };
            near_now.insert(player);
            if !self.alerts.enemies_near.contains(&player) {
                self.raise_alert(player, AlertKind::EnemyNear, pos, None);
            }
        }
        self.alerts.enemies_near = near_now;
    }

    /// Ein Gebäude von `owner` hat Schaden genommen (aus `combat.rs`).
    pub(crate) fn alert_building_attacked(&mut self, owner: u32, kind: BuildingKind, at: (f32, f32)) {
        self.raise_alert(owner, AlertKind::BuildingAttacked, at, Some(kind));
    }

    /// Warnung schicken, wenn diese Art für den Spieler gerade nicht gesperrt ist.
    pub(super) fn raise_alert(&mut self, player: u32, alert: AlertKind, at: (f32, f32), building: Option<BuildingKind>) {
        if player == 0 || self.alerts.cooldown.contains_key(&(player, alert)) {
            return;
        }
        let seconds = self.economy.def.army.alert_seconds;
        if seconds > 0.0 {
            self.alerts.cooldown.insert((player, alert), seconds);
        }
        self.events.push(ServerMsg::Alert { player, alert, x: at.0, y: at.1, building });
    }
}

#[cfg(test)]
mod tests {
    use protocol::{AlertKind, BuildingKind, ServerMsg, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    fn world() -> World {
        let row = "0".repeat(120);
        let text = format!("spawn 1 1\nspawn 118 118\n{}", format!("{row}\n").repeat(120));
        let mut eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        // Feste Werte, unabhängig vom Balancing.
        eco.def.army.alert_radius = 80.0;
        eco.def.army.alert_seconds = 30.0;
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.spawn_player(2);
        w.place_building(1, BuildingKind::Keep, 10, 10).unwrap();
        w.take_events();
        w
    }

    fn put(w: &mut World, owner: u32, kind: UnitKind, pos: (f32, f32)) -> usize {
        let id = 10_000 + w.units.len() as u32;
        w.units.push(Unit::new(id, owner, kind, pos));
        w.units.len() - 1
    }

    fn alerts(w: &mut World, kind: AlertKind) -> Vec<u32> {
        w.take_events()
            .into_iter()
            .filter_map(|e| match e {
                ServerMsg::Alert { player, alert, .. } if alert == kind => Some(player),
                _ => None,
            })
            .collect()
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds / 0.05) as usize {
            w.step_alerts(0.05);
        }
    }

    #[test]
    fn feinde_nahe_der_burg_warnen_einmal_je_vorfall() {
        let mut w = world();
        // Weit weg: nichts.
        let e = put(&mut w, 2, UnitKind::Spearman, (110.5, 110.5));
        run(&mut w, 2.0);
        assert!(alerts(&mut w, AlertKind::EnemyNear).is_empty());
        // Kommt nah: genau eine Warnung, auch wenn er bleibt.
        w.units[e].pos = (40.5, 40.5);
        run(&mut w, 20.0);
        assert_eq!(alerts(&mut w, AlertKind::EnemyNear), vec![1]);
        // Geht und kommt nach der Sperre wieder: neue Warnung.
        w.units[e].pos = (110.5, 110.5);
        run(&mut w, 40.0);
        w.units[e].pos = (40.5, 40.5);
        run(&mut w, 2.0);
        assert_eq!(alerts(&mut w, AlertKind::EnemyNear), vec![1]);
    }

    #[test]
    fn eigene_und_zivile_einheiten_warnen_nicht() {
        let mut w = world();
        put(&mut w, 1, UnitKind::Spearman, (15.5, 15.5));
        put(&mut w, 2, UnitKind::Worker, (15.5, 15.5));
        run(&mut w, 3.0);
        assert!(alerts(&mut w, AlertKind::EnemyNear).is_empty());
    }

    #[test]
    fn angriff_auf_gebaeude_warnt_mit_sperre() {
        let mut w = world();
        w.alert_building_attacked(1, BuildingKind::Keep, (12.0, 12.0));
        w.alert_building_attacked(1, BuildingKind::Keep, (12.0, 12.0));
        assert_eq!(alerts(&mut w, AlertKind::BuildingAttacked), vec![1]);
        run(&mut w, 31.0);
        w.alert_building_attacked(1, BuildingKind::Keep, (12.0, 12.0));
        assert_eq!(alerts(&mut w, AlertKind::BuildingAttacked), vec![1]);
    }
}
