//! Abbau: Holzfäller, Steinbruch und Mine haben Arbeiter, die zum Rohstoff
//! laufen, dort `seconds` lang arbeiten und die Ware zurücktragen.
//!
//! Zurück am Betrieb legt der Arbeiter die Ware ab, wenn es Träger oder Karren
//! gibt, die sie abholen, und Platz ist. Sonst trägt er sie selbst ins nächste
//! Lager und baut in der Zeit nichts ab.

use std::collections::{HashMap, HashSet};

use protocol::{BuildingStatus, UnitKind};

use super::World;

/// Was ein Arbeiter gerade tut.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Gather {
    /// Unterwegs zum Rohstoff-Feld.
    ToTile(usize),
    /// Arbeitet am Feld, noch so viele Sekunden.
    Working { tile: usize, left: f32 },
    /// Trägt die Ware zum eigenen Betrieb.
    Home,
    /// Trägt die Ware selbst ins Lager oder direkt zu einem Betrieb, der sie gerade braucht (Gebäude-Id).
    Deliver(u32),
    /// Herstellungsbetrieb: holt Eingänge aus diesem Lager (siehe `errand.rs`).
    Fetch(u32),
}

/// Wie lange ein Arbeiter wartet, bevor er es nochmal versucht.
const RETRY: f32 = 2.0;

impl World {
    pub(super) fn step_gather(&mut self, dt: f32) {
        let index: HashMap<u32, usize> = self.buildings.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
        let mut busy: HashMap<u32, BuildingStatus> = HashMap::new();
        let mut present: HashSet<u32> = HashSet::new();
        for k in 0..self.units.len() {
            if self.units[k].kind != UnitKind::Worker {
                continue;
            }
            let Some(home) = self.units[k].home else { continue };
            let Some(&bi) = index.get(&home) else { continue };
            let gathers = self.economy.building(self.buildings[bi].kind).is_some_and(|d| !d.gathers.is_empty());
            if !gathers {
                // Herstellungsbetrieb: unterwegs = der Betrieb steht, solange keiner da ist.
                if self.step_errand(k, bi, dt) {
                    present.insert(home);
                } else {
                    busy.entry(home).or_insert(BuildingStatus::WorkerAway);
                }
                continue;
            }
            let status = self.step_worker(k, bi, dt);
            // Arbeitet einer, arbeitet der Betrieb.
            let entry = busy.entry(home).or_insert(status);
            if status == BuildingStatus::Working {
                *entry = status;
            }
        }
        busy.retain(|id, s| *s != BuildingStatus::WorkerAway || !present.contains(id));
        self.gather_status = busy;
    }

    fn step_worker(&mut self, k: usize, bi: usize, dt: f32) -> BuildingStatus {
        let b = &self.buildings[bi];
        let owner = b.owner;
        let recipe = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).cloned();
        let stopped = b.settings.paused || self.unstaffed.contains(&b.id) || recipe.is_none();
        let u = &mut self.units[k];
        if !u.path.is_empty() {
            return BuildingStatus::Working;
        }
        match u.gather {
            Some(Gather::ToTile(tile)) => {
                let seconds = recipe.map_or(1.0, |r| r.seconds);
                u.gather = Some(Gather::Working { tile, left: seconds });
                BuildingStatus::Working
            }
            Some(Gather::Working { tile, left }) => {
                let left = left - dt * self.unit_hunger_factor(k);
                if left > 0.0 {
                    self.units[k].gather = Some(Gather::Working { tile, left });
                    return BuildingStatus::Working;
                }
                if self.resource_left[tile] == 0 {
                    // Ein anderer war schneller: neues Feld suchen.
                    self.units[k].gather = None;
                    return BuildingStatus::Working;
                }
                self.deplete(tile);
                let out = recipe.and_then(|r| r.outputs.first().copied());
                self.units[k].carrying = out;
                self.go_to_building(k, bi);
                self.units[k].gather = Some(Gather::Home);
                BuildingStatus::Working
            }
            Some(Gather::Home) => self.drop_off(k, bi),
            Some(Gather::Deliver(target)) => {
                let was_hub = self.unload_errand(k, target);
                if let Some(load) = self.units[k].carrying {
                    // Abnehmer hatte doch keinen Platz mehr: Rest ins Lager.
                    let pos = self.units[k].pos;
                    if !was_hub {
                        if let Some(hub) = self.nearest_hub(owner, pos, load.good) {
                            if self.walk_to(k, hub) {
                                self.units[k].gather = Some(Gather::Deliver(hub));
                                return BuildingStatus::Working;
                            }
                        }
                    }
                    // Lager voll: Rest wieder mitnehmen und später nochmal versuchen.
                    self.units[k].cooldown = RETRY;
                    self.go_to_building(k, bi);
                    self.units[k].gather = Some(Gather::Home);
                    return BuildingStatus::NoStorageRoom;
                }
                self.units[k].gather = None;
                BuildingStatus::Working
            }
            Some(Gather::Fetch(_)) => {
                self.units[k].gather = None; // nur für Herstellungsbetriebe
                BuildingStatus::Working
            }
            None => {
                if self.units[k].carrying.is_some() {
                    return self.drop_off(k, bi);
                }
                let u = &mut self.units[k];
                u.cooldown -= dt;
                if stopped {
                    self.go_to_building(k, bi);
                    return if self.unstaffed.contains(&self.buildings[bi].id) {
                        BuildingStatus::NoWorkers
                    } else if self.buildings[bi].settings.paused {
                        BuildingStatus::Paused
                    } else {
                        BuildingStatus::Idle
                    };
                }
                if u.cooldown > 0.0 {
                    // Wartet: Zustand bleibt, wie er war (z. B. kein Rohstoff).
                    return self.buildings[bi].status;
                }
                if self.target_reached(bi) {
                    self.units[k].cooldown = RETRY;
                    self.go_to_building(k, bi);
                    return BuildingStatus::TargetReached;
                }
                // Das Feld, zu dem man am schnellsten hinkommt (nicht nur nach Luftlinie).
                let Some((tile, stand)) = self.fastest_gather_tile(bi) else {
                    self.units[k].cooldown = RETRY;
                    self.go_to_building(k, bi);
                    return BuildingStatus::NoResource;
                };
                let pos = self.units[k].pos;
                let stand = stand.or_else(|| self.stand_at(tile, pos)).or_else(|| self.door(&self.buildings[bi], pos));
                let Some(stand) = stand else {
                    self.units[k].cooldown = RETRY;
                    return BuildingStatus::NoResource;
                };
                let u = &mut self.units[k];
                u.path = self.map.nav.waypoints(pos, stand);
                u.target = Some(stand);
                u.gather = Some(Gather::ToTile(tile));
                BuildingStatus::Working
            }
        }
    }

    /// Am Betrieb angekommen: ablegen, wenn Träger es holen, sonst selbst ins Lager bringen.
    fn drop_off(&mut self, k: usize, bi: usize) -> BuildingStatus {
        let Some(load) = self.units[k].carrying else {
            self.units[k].gather = None;
            return BuildingStatus::Working;
        };
        let b = &self.buildings[bi];
        let (id, owner) = (b.id, b.owner);
        let carriers = !b.settings.worker_carries
            && self.units.iter().any(|u| u.owner == owner && u.is_transport() && !self.idle_units.contains(&u.id));
        let room = self.inventories.get(&id).is_some_and(|inv| load.amount <= inv.room(load.good));
        if carriers && room {
            self.inventories.get_mut(&id).expect("eben geprüft").add(load.good, load.amount);
            self.dirty_buildings.insert(id);
            self.units[k].carrying = None;
            self.units[k].gather = None;
            return BuildingStatus::Working;
        }
        let pos = self.units[k].pos;
        let hub = self.nearest_hub(owner, pos, load.good);
        // Braucht ein anderer Betrieb die Ware gerade? Dann direkt dorthin statt ins Lager.
        let direct = self.worker_direct_target(bi, pos, load.good, hub).filter(|&(_, need)| need >= load.amount).map(|(to, _)| to);
        for target in direct.into_iter().chain(hub) {
            if self.walk_to(k, target) {
                self.units[k].gather = Some(Gather::Deliver(target));
                return BuildingStatus::Working;
            }
        }
        // Kein Lager: dann eben im Betrieb lassen, solange Platz ist.
        if room {
            self.inventories.get_mut(&id).expect("eben geprüft").add(load.good, load.amount);
            self.dirty_buildings.insert(id);
            self.units[k].carrying = None;
            self.units[k].gather = None;
            return BuildingStatus::Working;
        }
        self.units[k].gather = Some(Gather::Home);
        self.full_status(owner, [load.good])
    }

    fn go_to_building(&mut self, k: usize, bi: usize) {
        let pos = self.units[k].pos;
        let b = &self.buildings[bi];
        let Some(door) = self.door(b, pos) else { return };
        if (door.0 - pos.0).powi(2) + (door.1 - pos.1).powi(2) < 0.25 {
            return;
        }
        let u = &mut self.units[k];
        u.path = self.map.nav.waypoints(pos, door);
        u.target = Some(door);
    }

    /// Freies Feld neben einem Rohstoff-Feld, `from` am nächsten.
    fn stand_at(&self, tile: usize, from: (f32, f32)) -> Option<(f32, f32)> {
        let w = self.map.width as i32;
        let (tx, ty) = (tile as i32 % w, tile as i32 / w);
        (-1..=1)
            .flat_map(|dy| (-1..=1).map(move |dx| (tx + dx, ty + dy)))
            .filter(|&(x, y)| (x, y) != (tx, ty) && self.map.nav.is_open(x, y))
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .min_by(|a, b| {
                let d = |p: &(f32, f32)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
                d(a).total_cmp(&d(b))
            })
    }

    /// Tür des nächsten eigenen Wohnhauses (Gebäude mit Wohnplatz auf seiner Stufe).
    pub(super) fn home_door(&self, owner: u32, except: u32, near: (f32, f32)) -> Option<(f32, f32)> {
        self.buildings
            .iter()
            .filter(|h| h.owner == owner && h.id != except)
            .filter(|h| {
                self.economy.building(h.kind).and_then(|d| d.levels.get(h.level.max(1) as usize - 1)).is_some_and(|l| l.housing > 0)
            })
            .min_by(|a, b| {
                let d = |h: &protocol::BuildingState| (h.x as f32 + h.w as f32 / 2.0 - near.0).hypot(h.y as f32 + h.h as f32 / 2.0 - near.1);
                d(a).total_cmp(&d(b))
            })
            .and_then(|h| self.door(h, near))
    }

    /// Betrieb bekommt (wieder) eine Wohnung für seine Arbeiter: sie kommen aus dem
    /// nächsten Wohnhaus und laufen zur Arbeit, statt plötzlich am Betrieb zu stehen.
    pub(super) fn send_workers_from_home(&mut self, building: u32) {
        let Some(b) = self.buildings.iter().find(|b| b.id == building).cloned() else { return };
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let (Some(from), Some(work)) = (self.home_door(b.owner, b.id, center), self.door(&b, center)) else { return };
        let path = self.map.nav.waypoints(from, work);
        // Wer noch eine Last trägt, bringt sie erst weg.
        for u in self.units.iter_mut().filter(|u| u.kind == UnitKind::Worker && u.home == Some(b.id) && u.carrying.is_none()) {
            u.pos = from;
            u.path = path.clone();
            u.layers.clear();
            u.on_wall = false;
            u.target = Some(work);
            u.gather = None;
        }
    }

    /// Arbeiter für einen neuen Betrieb (mindestens einer).
    pub(super) fn spawn_workers(&mut self, bi: usize) {
        let n = self.worker_slots(&self.buildings[bi]);
        self.spawn_worker_units(bi, n);
    }

    /// So viele Arbeiter gehören zu diesem Gebäude (Abbau- und Herstellungsbetriebe;
    /// Mauern, Lager usw. haben keine).
    pub(super) fn worker_slots(&self, b: &protocol::BuildingState) -> u32 {
        let Some(def) = self.economy.building(b.kind) else { return 0 };
        if def.gathers.is_empty() && def.levels.iter().all(|l| l.recipes.is_empty()) {
            return 0;
        }
        def.at(b.level).workers.max(1)
    }

    /// `count` neue Arbeiter für das Gebäude `bi`.
    pub(super) fn spawn_worker_units(&mut self, bi: usize, count: u32) {
        let b = self.buildings[bi].clone();
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let Some(work) = self.door(&b, center) else { return };
        // Neue Arbeiter kommen aus dem nächsten eigenen Wohnhaus (ohne Wohnhaus
        // am Bergfried, sonst am nächsten Lager) und laufen von dort zu ihrem Betrieb.
        let start = self.home_door(b.owner, b.id, center).or_else(|| {
            self.buildings
                .iter()
                .filter(|h| h.owner == b.owner && h.id != b.id && self.economy.building(h.kind).is_some_and(|d| d.hub))
                .min_by_key(|h| {
                    let keep = h.kind != protocol::BuildingKind::Keep;
                    let d = (h.x as f32 + h.w as f32 / 2.0 - center.0).hypot(h.y as f32 + h.h as f32 / 2.0 - center.1);
                    (keep, d as u32)
                })
                .and_then(|h| self.door(h, center))
        });
        for _ in 0..count {
            let id = self.next_unit_id;
            self.next_unit_id += 1;
            let mut u = super::Unit::new(id, b.owner, UnitKind::Worker, start.unwrap_or(work));
            u.home = Some(b.id);
            if let Some(from) = start {
                u.path = self.map.nav.waypoints(from, work);
                u.target = Some(work);
            }
            self.units.push(u);
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, BuildingStatus, Good, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn world() -> World {
        // Wald ganz rechts, Bergfried links, Holzfäller dazwischen.
        let mut text = String::from("spawn 0 9\n[terrain]\n");
        for _ in 0..12 {
            text += &format!("{}\n", "0".repeat(30));
        }
        text += "[resources]\n";
        for y in 0..12 {
            text += &format!("{}{}\n", ".".repeat(28), if y < 4 { "WW" } else { ".." });
        }
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w
    }

    fn run(w: &mut World, s: f32) {
        for _ in 0..(s / 0.05) as usize {
            w.step(0.05);
        }
    }

    #[test]
    fn arbeiter_laeuft_hin_und_bringt_holz() {
        let mut w = world();
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let b = w.place_building(1, BuildingKind::Woodcutter, 18, 2).unwrap();
        assert_eq!(w.units.iter().filter(|u| u.kind == UnitKind::Worker).count(), 1);
        let worker = w.units.iter().find(|u| u.kind == UnitKind::Worker).unwrap();
        assert!(worker.pos.0 < 10.0, "kommt am Bergfried an, nicht im Betrieb: {:?}", worker.pos);
        assert!(!worker.path.is_empty(), "läuft zum Betrieb");
        let before = w.stocks[&1].amount(Good::Wood);
        run(&mut w, 2.0);
        let worker = w.units.iter().find(|u| u.kind == UnitKind::Worker).unwrap();
        assert!(!worker.path.is_empty(), "läuft zum Wald");
        assert!(w.buildings.iter().any(|x| x.id == b.id && x.status == BuildingStatus::Working));
        run(&mut w, 120.0);
        assert!(w.stocks[&1].amount(Good::Wood) > before, "Holz kommt im Vorrat an");
    }

    #[test]
    fn steinbruch_baut_im_umkreis_ab() {
        // Stein nur rechts außen; der Steinbruch steht zwei Felder daneben.
        let mut text = String::from("spawn 0 9\n[terrain]\n");
        for _ in 0..12 {
            text += &format!("{}\n", "0".repeat(30));
        }
        text += "[resources]\n";
        for y in 0..12 {
            text += &format!("{}{}\n", ".".repeat(28), if y < 4 { "SS" } else { ".." });
        }
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let r = w.economy.building(BuildingKind::Quarry).unwrap().at(1).gather_radius;
        assert!(r >= 2, "kleiner Umkreis eingestellt");
        // Zu weit weg geht nicht, daneben schon.
        assert_eq!(w.place_building(1, BuildingKind::Quarry, 24 - r, 6).unwrap_err().key, "error.needs_resource");
        w.place_building(1, BuildingKind::Quarry, 24, 1).unwrap();
        let before = w.stocks[&1].amount(Good::Stone);
        run(&mut w, 150.0);
        assert!(w.stocks[&1].amount(Good::Stone) > before, "Stein kommt im Vorrat an");
    }

    #[test]
    fn ohne_traeger_traegt_der_arbeiter_selbst() {
        let mut w = world();
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        w.economy.def.army.civilian_respawn_seconds = 0.0; // entfernte Träger nicht ersetzen
        w.units.retain(|u| !u.is_transport());
        let b = w.place_building(1, BuildingKind::Woodcutter, 18, 2).unwrap();
        let before = w.stocks[&1].amount(Good::Wood);
        // Erstes Stück: hin, abbauen, zurück, dann weiter zum Bergfried.
        let mut saw_delivery = false;
        for _ in 0..(150.0 / 0.05) as usize {
            w.step(0.05);
            let u = w.units.iter().find(|u| u.kind == UnitKind::Worker).unwrap();
            if matches!(u.gather, Some(super::Gather::Deliver(_))) {
                saw_delivery = true;
                assert!(u.carrying.is_some());
            }
        }
        assert!(saw_delivery, "der Arbeiter geht selbst zum Lager");
        assert!(w.stocks[&1].amount(Good::Wood) > before);
        assert_eq!(w.inventories[&b.id].amount(Good::Wood), 0, "nichts bleibt im Betrieb liegen");
    }
}
