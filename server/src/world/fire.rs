//! Feuer: Gebäude brennen (`BuildingState::fire`, Stärke bis 1), nehmen Feuerschaden gegen ihre
//! Feuer-Rüstung, und der Brand springt auf Nachbarn über. Angezündet wird von Soldaten mit
//! Fackel (`UnitDef::ignite_seconds`, Befehl „Anzünden“) oder von selbst: Betriebe mit
//! `fire_risk` (Schmiede, Schmelze) fangen beim Arbeiten ab und zu Feuer. Gelöscht wird nur
//! von Feuerwehrleuten: den Arbeitern einer Feuerwache. Sie laufen zu Bränden eigener und
//! verbündeter Gebäude im Umkreis `service_radius` der Wache, löschen, solange ihr Wasser
//! reicht (`fire.water_seconds`), und holen an der Wache neues.

use std::collections::HashMap;

use protocol::{AlertKind, AttackTarget, BuildingKind, DamageType};

use super::World;

/// Wie oft (Sekunden) Schaden und Überspringen gerechnet werden.
const TICK: f32 = 1.0;
/// So nah (Felder vom Rand) muss ein Feuerwehrmann am Gebäude stehen, um zu löschen.
const SPRAY_REACH: f32 = 1.6;
/// Kein Weg zum Brand: so lange wartet ein Feuerwehrmann, bevor er es nochmal versucht.
const RETRY: f32 = 3.0;

/// Was ein Feuerwehrmann gerade tut (siehe `Unit::fire_job`).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct FireJob {
    /// Brennendes Gebäude, zu dem er läuft oder das er löscht.
    pub target: Option<u32>,
    /// Sekunden Wasser, die er noch hat.
    pub water: f32,
    /// Löscht gerade (für die Anzeige).
    pub spraying: bool,
}

/// Abstand von `p` zum Rand des Gebäudes (0 = darauf).
fn gap(b: &protocol::BuildingState, p: (f32, f32)) -> f32 {
    let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
    let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
    dx.hypot(dy)
}

/// Abstand zwischen den Rändern zweier Gebäude (0 = sie stoßen aneinander).
fn rect_gap(a: &protocol::BuildingState, b: &protocol::BuildingState) -> f32 {
    let dx = (a.x as f32 - (b.x + b.w) as f32).max(b.x as f32 - (a.x + a.w) as f32).max(0.0);
    let dy = (a.y as f32 - (b.y + b.h) as f32).max(b.y as f32 - (a.y + a.h) as f32).max(0.0);
    dx.hypot(dy)
}

fn center(b: &protocol::BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

impl World {
    /// Kann dieses Gebäude brennen? Nicht, wenn seine Rüstung Feuer ganz abwehrt (Stein),
    /// nicht bei Wegen, Brücken und Baustellen im Feld, nicht als Ruine.
    pub(crate) fn burnable(&self, b: &protocol::BuildingState) -> bool {
        if b.hp == 0 || matches!(b.kind, BuildingKind::Bridge | BuildingKind::SiegeSite) {
            return false;
        }
        let Some(def) = self.economy.building(b.kind) else { return false };
        if def.is_road() {
            return false;
        }
        self.economy.damage_against(100, DamageType::Fire, &def.at(b.level).armor) > 0
    }

    /// Gebäude fängt Feuer (falls es brennen kann und nicht schon brennt).
    pub(crate) fn set_on_fire(&mut self, id: u32) -> bool {
        let start = self.economy.def.fire.start.clamp(0.01, 1.0);
        let Some(i) = self.buildings.iter().position(|b| b.id == id) else { return false };
        if self.buildings[i].fire > 0.0 || !self.burnable(&self.buildings[i]) {
            return false;
        }
        let b = &mut self.buildings[i];
        b.fire = start;
        let (owner, kind, at) = (b.owner, b.kind, center(b));
        self.dirty_buildings.insert(id);
        self.raise_alert(owner, AlertKind::Fire, at, Some(kind));
        true
    }

    /// Befehl „Anzünden“: Wer eine Fackel hat (`ignite_seconds`), läuft zum Gebäude und zündet
    /// es an. Die anderen laufen nur hin. Ohne `force` nur feindliche Gebäude.
    pub fn order_ignite(&mut self, player: u32, unit_ids: &[u32], target: AttackTarget, force: bool) -> bool {
        let AttackTarget::Building { id } = target else { return self.order_attack_full(player, unit_ids, target, force) };
        let Some(b) = self.buildings.iter().find(|b| b.id == id && b.hp > 0) else { return false };
        let (owner, pos) = (b.owner, center(b));
        let ok = (force || self.can_attack(player, owner)) && self.burnable(b) && b.fire <= 0.0;
        let mut movers = Vec::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.owner != player || u.is_civilian() || !unit_ids.contains(&u.id) {
                continue;
            }
            let secs = self.economy.unit(u.kind).map_or(0.0, |d| d.ignite_seconds);
            if ok && secs > 0.0 && !u.kind.is_siege() {
                let u = &mut self.units[k];
                u.attack_order = Some(target);
                u.attack_force = force;
                u.ignite = true;
                u.ignite_left = secs;
                u.resume_order = None;
                u.waypoints.clear();
                u.chase_timer = 0.0;
                u.speed_cap = None;
            } else {
                movers.push(u.id);
            }
        }
        if !movers.is_empty() {
            self.order_move(player, &movers, pos);
        }
        true
    }

    /// Anzünden am Gebäude (aus `step_combat`, wenn die Einheit dort angekommen ist).
    /// `true`, solange sie weiter an der Fackel bleibt.
    pub(super) fn step_ignite(&mut self, k: usize, building: u32, dt: f32) {
        let burning = self.buildings.iter().find(|b| b.id == building).is_none_or(|b| b.fire > 0.0 || !self.burnable(b));
        let u = &mut self.units[k];
        if burning {
            // Brennt schon (oder kann nicht brennen): fertig, stehen bleiben.
            u.attack_order = None;
            u.ignite = false;
            u.torch = false;
            return;
        }
        u.torch = true;
        u.ignite_left -= dt;
        if u.ignite_left > 0.0 {
            return;
        }
        u.attack_order = None;
        u.ignite = false;
        u.torch = false;
        self.set_on_fire(building);
    }

    pub(super) fn step_fire(&mut self, dt: f32) {
        let fire = self.economy.def.fire.clone();
        // Von selbst: arbeitende Betriebe mit Brandgefahr.
        let risky: Vec<(u32, f32)> = self
            .buildings
            .iter()
            .filter(|b| b.fire <= 0.0 && b.status == protocol::BuildingStatus::Working)
            .filter_map(|b| self.economy.building(b.kind).map(|d| (b.id, d.at(b.level).fire_risk)))
            .filter(|&(_, r)| r > 0.0)
            .collect();
        for (id, risk) in risky {
            if self.rand01() < risk / 100.0 * dt / 60.0 {
                self.set_on_fire(id);
            }
        }
        self.step_firefighters(dt, &fire);
        if !self.buildings.iter().any(|b| b.fire > 0.0) {
            self.fire_clock = 0.0;
            return;
        }
        // Brände werden stärker (zwischen den Takten nur für die Anzeige).
        for b in self.buildings.iter_mut().filter(|b| b.fire > 0.0) {
            let before = (b.fire * 20.0).round();
            b.fire = (b.fire + fire.grow_per_second * dt).min(1.0);
            if (b.fire * 20.0).round() != before {
                self.dirty_buildings.insert(b.id);
            }
        }
        self.fire_clock += dt;
        if self.fire_clock < TICK {
            return;
        }
        let secs = self.fire_clock;
        self.fire_clock = 0.0;
        // Schaden: je Takt einmal, mit der Rüstung gegen Feuer.
        let burning: Vec<(u32, f32)> = self.buildings.iter().filter(|b| b.fire > 0.0).map(|b| (b.id, b.fire)).collect();
        let mut destroyed = Vec::new();
        for &(id, strength) in &burning {
            let Some(i) = self.buildings.iter().position(|b| b.id == id) else { continue };
            if !self.burnable(&self.buildings[i]) {
                self.buildings[i].fire = 0.0;
                self.dirty_buildings.insert(id);
                continue;
            }
            let Some(def) = self.economy.building(self.buildings[i].kind) else { continue };
            let x = (fire.damage_per_second * strength * secs).round().max(1.0) as u32;
            let y = self.economy.damage_against(x, DamageType::Fire, &def.at(self.buildings[i].level).armor);
            let b = &mut self.buildings[i];
            b.hp = b.hp.saturating_sub(y);
            self.dirty_buildings.insert(id);
            if b.hp == 0 {
                b.fire = 0.0;
                if b.kind == BuildingKind::Keep {
                    // Der Bergfried bleibt als Ruine stehen (wie im Kampf).
                    self.replan_all();
                } else {
                    destroyed.push(id);
                }
            }
        }
        // Überspringen auf Nachbarn: je Nachbar eine Chance, mit der Stärke des Brandes.
        let mut catch = Vec::new();
        for &(id, strength) in &burning {
            if strength < fire.spread_from || destroyed.contains(&id) {
                continue;
            }
            let Some(src) = self.buildings.iter().find(|b| b.id == id) else { continue };
            for o in &self.buildings {
                if o.id == id || o.fire > 0.0 || catch.contains(&o.id) || rect_gap(src, o) > fire.spread_distance || !self.burnable(o) {
                    continue;
                }
                catch.push(o.id);
            }
        }
        let chance = fire.spread_percent / 100.0 * secs;
        let mut lit = Vec::new();
        for id in catch {
            // Stärkster Brand in der Nähe zählt.
            let Some(o) = self.buildings.iter().find(|b| b.id == id) else { continue };
            let strength = burning
                .iter()
                .filter(|(s, _)| !destroyed.contains(s))
                .filter_map(|&(s, f)| self.buildings.iter().find(|b| b.id == s).map(|b| (b, f)))
                .filter(|(b, f)| *f >= fire.spread_from && rect_gap(b, o) <= fire.spread_distance)
                .map(|(_, f)| f)
                .fold(0.0, f32::max);
            if self.rand01() < chance * strength {
                lit.push(id);
            }
        }
        for id in destroyed {
            self.destroy_building(id);
        }
        for id in lit {
            self.set_on_fire(id);
        }
    }

    /// Feuerwehrleute: Arbeiter der Feuerwachen.
    fn step_firefighters(&mut self, dt: f32, fire: &protocol::FireDef) {
        let stations: HashMap<u32, (u32, (f32, f32), f32)> = self
            .buildings
            .iter()
            .filter(|b| b.kind == BuildingKind::FireStation)
            .map(|b| (b.id, (b.owner, center(b), self.economy.building(b.kind).map_or(0, |d| d.at(b.level).service_radius) as f32)))
            .collect();
        if stations.is_empty() {
            return;
        }
        let any_fire = self.buildings.iter().any(|b| b.fire > 0.0);
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.kind != protocol::UnitKind::Worker {
                continue;
            }
            let Some(home) = u.home else { continue };
            let Some(&(owner, at, radius)) = stations.get(&home) else { continue };
            let unstaffed = self.unstaffed.contains(&home);
            let u = &mut self.units[k];
            let job = u.fire_job.get_or_insert_with(|| FireJob { target: None, water: fire.water_seconds, spraying: false });
            job.spraying = false;
            if unstaffed {
                job.target = None;
                continue;
            }
            u.cooldown = (u.cooldown - dt).max(0.0);
            // Noch gültiges Ziel?
            let target = job.target.filter(|t| self.buildings.iter().any(|b| b.id == *t && b.fire > 0.0));
            let job = self.units[k].fire_job.as_mut().expect("eben gesetzt");
            job.target = target;
            let water = job.water;
            // Kein Wasser mehr: zurück zur Wache, dort auffüllen.
            if water <= 0.0 || (target.is_none() && !any_fire) || (target.is_none() && self.units[k].cooldown > 0.0) {
                self.firefighter_home(k, home, fire.water_seconds);
                continue;
            }
            let target = match target {
                Some(t) => t,
                None => {
                    // Nächster Brand eines eigenen oder verbündeten Gebäudes im Umkreis der Wache.
                    let pos = self.units[k].pos;
                    let found = self
                        .buildings
                        .iter()
                        .filter(|b| b.fire > 0.0 && (b.owner == owner || self.allied(b.owner, owner)))
                        .filter(|b| radius <= 0.0 || gap(b, at) <= radius)
                        .min_by(|a, b| gap(a, pos).total_cmp(&gap(b, pos)))
                        .map(|b| b.id);
                    match found {
                        Some(t) => {
                            self.units[k].fire_job.as_mut().expect("gesetzt").target = Some(t);
                            self.units[k].path.clear();
                            t
                        }
                        None => {
                            self.firefighter_home(k, home, fire.water_seconds);
                            continue;
                        }
                    }
                }
            };
            let Some(b) = self.buildings.iter().find(|b| b.id == target) else { continue };
            let pos = self.units[k].pos;
            if gap(b, pos) <= SPRAY_REACH {
                // Löschen, solange das Wasser reicht (Hunger und Rationen wirken mit).
                let speed = self.unit_hunger_factor(k);
                let u = &mut self.units[k];
                u.path.clear();
                u.target = Some(u.pos);
                let job = u.fire_job.as_mut().expect("gesetzt");
                job.spraying = true;
                job.water -= dt;
                let b = self.buildings.iter_mut().find(|b| b.id == target).expect("eben gefunden");
                let before = (b.fire * 20.0).round();
                b.fire = (b.fire - fire.extinguish_per_second * speed * dt).max(0.0);
                if b.fire <= 0.0 || (b.fire * 20.0).round() != before {
                    self.dirty_buildings.insert(target);
                }
                continue;
            }
            if !self.units[k].path.is_empty() {
                continue;
            }
            // Hinlaufen: zum nächsten freien Feld am Gebäude.
            let goal = self.foot_of(center(b), pos);
            let path = self.map.nav.waypoints(pos, goal);
            let u = &mut self.units[k];
            if path.is_empty() {
                // Kein Weg: aufgeben und später nochmal schauen.
                u.cooldown = RETRY;
                if let Some(j) = u.fire_job.as_mut() {
                    j.target = None;
                }
                continue;
            }
            u.path = path;
            u.layers.clear();
            u.on_wall = false;
            u.target = Some(goal);
        }
    }

    /// Feuerwehrmann zurück an die Wache; an der Tür füllt er sein Wasser auf.
    fn firefighter_home(&mut self, k: usize, home: u32, full: f32) {
        if let Some(j) = self.units[k].fire_job.as_mut() {
            j.target = None;
        }
        let Some(b) = self.buildings.iter().find(|b| b.id == home) else { return };
        if self.at_door(b, self.units[k].pos) {
            if let Some(j) = self.units[k].fire_job.as_mut() {
                j.water = full;
            }
            return;
        }
        if self.units[k].path.is_empty() {
            self.walk_to(k, home);
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{Armor, BuildingKind, DamageType, UnitKind};

    use crate::economy::Economy;
    use crate::map::GameMap;
    use crate::world::World;

    fn world() -> World {
        let mut e = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people().without_build_radius();
        e.def.start_stock = vec![
            protocol::GoodAmount { good: protocol::Good::Wood, amount: 900 },
            protocol::GoodAmount { good: protocol::Good::Boards, amount: 900 },
            protocol::GoodAmount { good: protocol::Good::Stone, amount: 900 },
        ];
        // Feste Werte, unabhängig vom Balancing.
        e.def.fire = protocol::FireDef {
            start: 0.2,
            grow_per_second: 0.02,
            damage_per_second: 10.0,
            spread_from: 0.4,
            spread_distance: 2.0,
            spread_percent: 100.0,
            extinguish_per_second: 0.2,
            water_seconds: 10.0,
        };
        for b in &mut e.def.buildings {
            for l in &mut b.levels {
                l.fire_risk = 0.0;
                l.armor.retain(|a| a.kind != DamageType::Fire);
            }
        }
        let row = "0".repeat(80);
        let text = format!("spawn 10 10\nspawn 70 70\n{}", format!("{row}\n").repeat(80));
        let mut w = World::new(GameMap::parse(&text).unwrap(), e);
        w.spawn_player(1);
        w.spawn_player(2);
        w
    }

    fn steps(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 10.0) as u32 {
            w.step(0.1);
        }
    }

    fn building(w: &World, id: u32) -> Option<&protocol::BuildingState> {
        w.buildings.iter().find(|b| b.id == id)
    }

    #[test]
    fn feuer_schadet_und_springt_ueber() {
        let mut w = world();
        let a = w.place_building(1, BuildingKind::House, 30, 30).unwrap().id;
        let b = w.place_building(1, BuildingKind::House, 34, 30).unwrap().id;
        let far = w.place_building(1, BuildingKind::House, 50, 30).unwrap().id;
        assert!(w.set_on_fire(a));
        let hp = building(&w, a).unwrap().hp;
        steps(&mut w, 15.0);
        assert!(building(&w, a).unwrap().hp < hp, "brennt und nimmt Schaden");
        assert!(building(&w, b).unwrap().fire > 0.0, "Nachbar fängt Feuer");
        assert_eq!(building(&w, far).unwrap().fire, 0.0, "weit weg bleibt es ruhig");
        steps(&mut w, 120.0);
        assert!(building(&w, a).is_none(), "abgebrannt");
    }

    #[test]
    fn stein_brennt_nicht() {
        let mut w = world();
        let id = w.place_building(1, BuildingKind::House, 30, 30).unwrap().id;
        let kind = building(&w, id).unwrap().kind;
        for l in &mut w.economy.def.buildings.iter_mut().find(|b| b.kind == kind).unwrap().levels {
            l.armor.push(Armor { kind: DamageType::Fire, min: 0, block: 1.0 });
        }
        assert!(!w.set_on_fire(id));
    }

    #[test]
    fn feuerwehr_loescht_im_umkreis() {
        let mut w = world();
        let station = w.place_building(1, BuildingKind::FireStation, 30, 40).unwrap().id;
        let house = w.place_building(1, BuildingKind::House, 30, 30).unwrap().id;
        steps(&mut w, 3.0);
        assert!(w.units.iter().any(|u| u.home == Some(station) && u.fire_job.is_some()), "Feuerwehrleute da");
        assert!(w.set_on_fire(house));
        steps(&mut w, 40.0);
        let h = building(&w, house).expect("steht noch");
        assert_eq!(h.fire, 0.0, "gelöscht");
        assert!(h.hp > 0);
    }

    #[test]
    fn soldat_zuendet_feindliches_gebaeude_an() {
        let mut w = world();
        let house = w.place_building(2, BuildingKind::House, 30, 30).unwrap().id;
        let id = w.next_unit_id;
        w.next_unit_id += 1;
        w.units.push(super::super::Unit::new(id, 1, UnitKind::Spearman, (28.5, 31.5)));
        w.order_ignite(1, &[id], protocol::AttackTarget::Building { id: house }, false);
        steps(&mut w, 10.0);
        assert!(building(&w, house).unwrap().fire > 0.0, "brennt");
        // Eigenes Gebäude ohne „Alles angreifen“: nicht.
        let own = w.place_building(1, BuildingKind::House, 40, 30).unwrap().id;
        w.order_ignite(1, &[id], protocol::AttackTarget::Building { id: own }, false);
        steps(&mut w, 15.0);
        assert_eq!(building(&w, own).unwrap().fire, 0.0);
    }

    #[test]
    fn schmiede_kann_von_selbst_brennen() {
        let mut w = world();
        let id = w.place_building(1, BuildingKind::House, 30, 30).unwrap().id;
        let kind = building(&w, id).unwrap().kind;
        w.economy.def.buildings.iter_mut().find(|b| b.kind == kind).unwrap().levels[0].fire_risk = 100.0 * 60.0;
        w.buildings.iter_mut().find(|b| b.id == id).unwrap().status = protocol::BuildingStatus::Working;
        w.step_fire(0.1);
        assert!(building(&w, id).unwrap().fire > 0.0);
    }
}
