//! Belagerung: Baumeister bauen Belagerungsgeräte im Feld und bedienen sie.
//!
//! - **Baustelle:** `build_siege` setzt eine Baustelle (Gebäude `SiegeSite`, Größe aus
//!   `siege.size`) an beliebiger Stelle, ohne Abstand zum Bergfried und auch mit Feinden in
//!   der Nähe. Die Balliste entsteht oben auf einem eigenen Turm: dann trägt der Turm die
//!   Baustelle (`BuildingState::site`). Was gebaut werden darf, schalten die Stufen der
//!   Belagerungswerkstatt frei (`BuildingLevel::siege`).
//! - **Baustoffe:** die Kosten des Geräts (`UnitDef::cost`) müssen zur Baustelle. Eigene Karren
//!   fahren sie aus dem Lager hin, egal wie weit (siehe `transport.rs`). Mit `backpack` nimmt
//!   jeder Baumeister gleich `army.builder_backpack` davon aus dem Vorrat mit.
//! - **Bauen:** jeder Baumeister, der an der Baustelle steht, schafft eine Sekunde Arbeit je
//!   Sekunde; fertig ist sie nach `UnitDef::seconds` Arbeit. Weiter als die gelieferten
//!   Baustoffe reichen, geht es nicht.
//! - **Bedienung:** Ein fertiges Gerät braucht `siege.crew` Baumeister in der Nähe, sonst
//!   schießt und fährt es nicht. Freie Baumeister daneben übernehmen das von selbst; ein
//!   Laufbefehl holt sie wieder weg. Rollt das Gerät, laufen sie mit.
//! - **Schießen:** Geschosse fliegen mit `siege.shot_speed` und streuen (`scatter`);
//!   Schaden gibt es erst beim Einschlag: am Gebäude auf dem Feld und an Einheiten im Umkreis
//!   `siege.splash` (0 = nur die nächste). Näher als `siege.min_range` schießt es nicht.

use std::collections::HashMap;

use protocol::{AttackTarget, BuildingKind, BuildingSettings, BuildingState, BuildingStatus, GateMode, GoodAmount, ServerMsg, Shot, SiteState, Text, UnitKind};

use super::{Refusal, Unit, World};

/// So nah (Felder vom Rand) muss ein Baumeister an der Baustelle stehen, um zu bauen.
const SITE_REACH: f32 = 2.0;
/// So nah (Felder) muss die Bedienung am Gerät stehen.
const CREW_REACH: f32 = 3.0;
/// So oft (Sekunden) geht der Baufortschritt an die Clients.
const SITE_REPORT_SECONDS: f32 = 0.5;

/// Ein Stein oder Bolzen eines Belagerungsgeräts in der Luft.
#[derive(Clone, Debug)]
pub(super) struct SiegeShot {
    shooter: u32,
    owner: u32,
    kind: UnitKind,
    to: (f32, f32),
    left: f32,
}

/// Abstand eines Punktes zum Rechteck des Gebäudes (0 = darin).
fn dist_to(b: &BuildingState, p: (f32, f32)) -> f32 {
    let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
    let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
    dx.hypot(dy)
}

fn center(b: &BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

fn amount(list: &[GoodAmount], good: protocol::Good) -> u32 {
    list.iter().filter(|g| g.good == good).map(|g| g.amount).sum()
}

fn add(list: &mut Vec<GoodAmount>, good: protocol::Good, n: u32) {
    match list.iter_mut().find(|g| g.good == good) {
        Some(g) => g.amount += n,
        None => list.push(GoodAmount { good, amount: n }),
    }
}

impl World {
    /// Wie viele Baumeister bedienen jedes Gerät (Id → Anzahl)?
    pub(super) fn crew_counts(&self) -> HashMap<u32, u32> {
        let mut m = HashMap::new();
        for u in &self.units {
            if let Some(d) = u.crew_of {
                *m.entry(d).or_insert(0) += 1;
            }
        }
        m
    }

    /// Steht das Gerät fest (Tribok, Balliste auf dem Turm)?
    pub(super) fn immobile(&self, u: &Unit) -> bool {
        u.kind.is_siege() && (u.perch.is_some() || self.economy.unit(u.kind).is_none_or(|d| d.speed <= 0.0))
    }

    /// Hat das Gerät genug Bedienung?
    pub(super) fn crewed(&self, u: &Unit, counts: &HashMap<u32, u32>) -> bool {
        let need = self.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).map_or(0, |s| s.crew);
        counts.get(&u.id).copied().unwrap_or(0) >= need
    }

    /// Darf der Spieler dieses Gerät bauen (eigene Belagerungswerkstatt mit passender Stufe)?
    pub fn siege_unlocked(&self, player: u32, kind: UnitKind) -> bool {
        self.buildings.iter().filter(|b| b.owner == player && b.hp > 0).any(|b| {
            self.economy
                .building(b.kind)
                .is_some_and(|d| d.levels.iter().take(b.level as usize).any(|l| l.siege.contains(&kind)))
        })
    }

    /// Passt eine Baustelle für dieses Gerät (nicht auf Türmen) hierher? Für die Computergegner.
    pub fn siege_site_free(&self, player: u32, kind: UnitKind, (x, y): (u32, u32)) -> bool {
        let Some(sd) = self.economy.unit(kind).and_then(|d| d.siege.as_ref()) else { return false };
        let Some(sdef) = self.economy.building(BuildingKind::SiegeSite) else { return false };
        let s = sd.size.max(1);
        if sd.on_towers || x + s > self.map.width || y + s > self.map.height || self.check_site(sdef, (x, y), (s, s), 0, &[]).is_err() {
            return false;
        }
        let covers = |p: (f32, f32)| p.0 >= x as f32 - 0.5 && p.1 >= y as f32 - 0.5 && p.0 < (x + s) as f32 + 0.5 && p.1 < (y + s) as f32 + 0.5;
        !self.units.iter().any(|u| !u.is_civilian() && !(u.kind == UnitKind::Builder && u.owner == player) && covers(u.pos))
    }

    /// Baumeister setzen eine Baustelle für ein Belagerungsgerät (siehe Modul-Text).
    pub fn build_siege(&mut self, player: u32, unit_ids: &[u32], kind: UnitKind, (x, y): (u32, u32), backpack: bool) -> Result<BuildingState, Refusal> {
        let def = self.economy.unit(kind).filter(|d| d.siege.is_some()).ok_or_else(|| Text::new("error.not_siege"))?.clone();
        let sd = def.siege.clone().expect("eben geprüft");
        let builders: Vec<usize> = (0..self.units.len())
            .filter(|&k| {
                let u = &self.units[k];
                u.owner == player && u.kind == UnitKind::Builder && unit_ids.contains(&u.id)
            })
            .collect();
        if builders.is_empty() {
            return Err(Text::new("error.no_builders"));
        }
        if !self.siege_unlocked(player, kind) {
            return Err(Text::new("error.siege_locked"));
        }
        if x >= self.map.width || y >= self.map.height {
            return Err(Text::new("error.outside_map"));
        }
        let mut needed: Vec<GoodAmount> = Vec::new();
        for c in &def.cost {
            add(&mut needed, c.good, c.amount);
        }
        let site = SiteState { unit: kind, needed, delivered: Vec::new(), work: 0.0, work_total: def.seconds.max(0.1), builders: 0 };
        let id = if sd.on_towers {
            let tile = (y * self.map.width + x) as usize;
            let id = self.occupied[tile];
            let b = self.buildings.iter_mut().find(|b| b.id == id).filter(|b| matches!(b.kind, BuildingKind::Tower | BuildingKind::HighTower));
            let Some(b) = b.filter(|b| b.owner == player && b.hp > 0) else { return Err(Text::new("error.needs_tower")) };
            if b.site.is_some() || self.units.iter().any(|u| u.perch == Some(id)) {
                return Err(Text::new("error.tower_taken"));
            }
            b.site = Some(site);
            self.dirty_buildings.insert(id);
            id
        } else {
            let sdef = self.economy.building(BuildingKind::SiegeSite).ok_or_else(|| Text::new("error.unknown_building"))?.clone();
            let s = sd.size.max(1);
            self.check_site(&sdef, (x, y), (s, s), 0, &[])?;
            let covers = |p: (f32, f32)| p.0 >= x as f32 && p.1 >= y as f32 && p.0 < (x + s) as f32 && p.1 < (y + s) as f32;
            if self.units.iter().any(|u| !u.is_civilian() && !(u.kind == UnitKind::Builder && u.owner == player) && covers(u.pos)) {
                return Err(Text::new("error.unit_in_way"));
            }
            let b = BuildingState {
                id: self.next_building_id,
                owner: player,
                kind: BuildingKind::SiegeSite,
                x,
                y,
                w: s,
                h: s,
                hp: sdef.at(1).max_hp,
                level: 1,
                settings: BuildingSettings {
                    recipe: None,
                    paused: false,
                    accept_deliveries: false,
                    stores: None,
                    keep_in_stock: Vec::new(),
                    gate: GateMode::Auto,
                    worker_carries: false,
                    cart_deliveries: true,
                    stop_at: Vec::new(),
                },
                status: BuildingStatus::Idle,
                rotation: 0,
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
                rally: None,
                site: Some(site),
            };
            self.next_building_id += 1;
            self.set_footprint(&b, true);
            self.buildings.push(b.clone());
            self.make_room(&b);
            // Baumeister, die auf der Fläche stehen, treten zur Seite.
            if let Some(door) = self.door(&b, center(&b)) {
                for &k in &builders {
                    if covers(self.units[k].pos) {
                        self.units[k].pos = door;
                    }
                }
            }
            self.replan_all();
            b.id
        };
        // Rucksack: jeder Baumeister nimmt, was noch fehlt (eine Ware je Baumeister).
        if backpack {
            let per = self.economy.def.army.builder_backpack;
            let mut planned: Vec<GoodAmount> = Vec::new();
            let site = self.buildings.iter().find(|b| b.id == id).and_then(|b| b.site.clone()).expect("eben gesetzt");
            for &k in &builders {
                if self.units[k].carrying.is_some() || per == 0 {
                    continue;
                }
                let Some(stock) = self.stocks.get_mut(&player) else { break };
                let pick = site.needed.iter().map(|g| (g.good, g.amount.saturating_sub(amount(&planned, g.good)).min(stock.amount(g.good)))).find(|&(_, n)| n > 0);
                let Some((good, n)) = pick else { break };
                let n = n.min(per);
                stock.take_all(&[GoodAmount { good, amount: n }]);
                add(&mut planned, good, n);
                self.units[k].carrying = Some(GoodAmount { good, amount: n });
                self.dirty_stocks.insert(player);
            }
        }
        // Hingehen: jeder auf ein eigenes freies Feld am Rand.
        let b = self.buildings.iter().find(|b| b.id == id).cloned().expect("eben gesetzt");
        let spots = self.ring_spots(&b);
        for (i, &k) in builders.iter().enumerate() {
            let uid = self.units[k].id;
            if spots.is_empty() {
                break;
            }
            let from = self.units[k].pos;
            let mut sorted = spots.clone();
            sorted.sort_by(|a, b| {
                let d = |p: &(f32, f32)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
                d(a).total_cmp(&d(b))
            });
            let spot = sorted[i % sorted.len()];
            if dist_to(&b, from) > SITE_REACH {
                self.order_move(player, &[uid], spot);
            }
        }
        Ok(b)
    }

    /// Freie Felder rund um ein Gebäude (Mitten).
    fn ring_spots(&self, b: &BuildingState) -> Vec<(f32, f32)> {
        let (x0, y0, x1, y1) = (b.x as i32 - 1, b.y as i32 - 1, (b.x + b.w) as i32, (b.y + b.h) as i32);
        (x0..=x1)
            .flat_map(|x| [(x, y0), (x, y1)])
            .chain((y0 + 1..y1).flat_map(|y| [(x0, y), (x1, y)]))
            .filter(|&(x, y)| self.map.nav.is_open(x, y))
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .collect()
    }

    /// Baustellen voranbringen, Bedienung zuweisen und mitlaufen lassen.
    pub(super) fn step_siege(&mut self, dt: f32) {
        let has_siege = self.units.iter().any(|u| u.kind.is_siege() || u.kind == UnitKind::Builder) || self.buildings.iter().any(|b| b.site.is_some());
        if !has_siege {
            if !self.storms.is_empty() {
                self.storms.clear();
                self.drop_from_walls();
            }
            return;
        }
        let index: HashMap<u32, usize> = self.units.iter().enumerate().map(|(k, u)| (u.id, k)).collect();
        // Bedienung eines verschwundenen Geräts ist frei.
        for k in 0..self.units.len() {
            if let Some(d) = self.units[k].crew_of {
                if !index.contains_key(&d) {
                    self.units[k].crew_of = None;
                }
            }
        }
        // Turm weg: die Balliste darauf ist hin.
        let lost: Vec<u32> = self
            .units
            .iter()
            .filter(|u| u.perch.is_some_and(|t| !self.buildings.iter().any(|b| b.id == t && b.hp > 0)))
            .map(|u| u.id)
            .collect();
        if !lost.is_empty() {
            self.units.retain(|u| !lost.contains(&u.id));
            self.events.push(ServerMsg::UnitsDied { unit_ids: lost });
            return;
        }
        // Feste Geräte fahren nicht.
        for k in 0..self.units.len() {
            if self.immobile(&self.units[k]) && !self.units[k].path.is_empty() {
                let u = &mut self.units[k];
                u.path.clear();
                u.layers.clear();
                u.waypoints.clear();
                u.target = Some(u.pos);
            }
        }
        self.step_sites(dt);
        self.assign_crew();
        self.step_towers(dt);
        self.crew_follow(dt);
    }

    fn step_sites(&mut self, dt: f32) {
        self.site_clock += dt;
        let report = self.site_clock >= SITE_REPORT_SECONDS;
        if report {
            self.site_clock = 0.0;
        }
        let sites: Vec<usize> = (0..self.buildings.len()).filter(|&i| self.buildings[i].site.is_some()).collect();
        let mut done = Vec::new();
        for bi in sites {
            let b = &self.buildings[bi];
            let near: Vec<usize> = (0..self.units.len())
                .filter(|&k| {
                    let u = &self.units[k];
                    u.owner == b.owner && u.kind == UnitKind::Builder && u.path.is_empty() && u.down <= 0.0 && u.crew_of.is_none() && dist_to(b, u.pos) <= SITE_REACH
                })
                .collect();
            let id = b.id;
            // Mitgebrachte Baustoffe abladen.
            for &k in &near {
                let Some(load) = self.units[k].carrying else { continue };
                let site = self.buildings[bi].site.as_mut().expect("Baustelle");
                let used = site.deliver(load);
                if used > 0 {
                    self.units[k].carrying = (used < load.amount).then_some(GoodAmount { good: load.good, amount: load.amount - used });
                    self.dirty_buildings.insert(id);
                }
            }
            let site = self.buildings[bi].site.as_mut().expect("Baustelle");
            let n = near.len() as u32;
            if site.builders != n {
                site.builders = n;
                self.dirty_buildings.insert(id);
            }
            let cap = site.work_total * site.supplied();
            if n > 0 && site.work < cap {
                site.work = (site.work + dt * n as f32).min(cap);
                if report {
                    self.dirty_buildings.insert(id);
                }
            }
            if site.work >= site.work_total - 1e-4 && site.supplied() >= 1.0 {
                done.push((id, near));
            }
        }
        for (id, near) in done {
            let ids: Vec<u32> = near.iter().map(|&k| self.units[k].id).collect();
            self.finish_site(id, &ids);
        }
    }

    /// Baustelle fertig: das Gerät steht da, die Baumeister daneben bedienen es.
    fn finish_site(&mut self, id: u32, builders: &[u32]) {
        let Some(b) = self.buildings.iter().find(|b| b.id == id).cloned() else { return };
        let Some(site) = b.site.clone() else { return };
        let uid = self.next_unit_id;
        self.next_unit_id += 1;
        let mut unit = Unit::new(uid, b.owner, site.unit, center(&b));
        if b.kind == BuildingKind::SiegeSite {
            self.remove_building(id);
            self.events.push(ServerMsg::BuildingRemoved { building_id: id });
            self.replan_all();
        } else {
            unit.on_wall = true;
            unit.perch = Some(id);
            if let Some(t) = self.buildings.iter_mut().find(|t| t.id == id) {
                t.site = None;
            }
            self.dirty_buildings.insert(id);
        }
        self.units.push(unit);
        let crew = self.economy.unit(site.unit).and_then(|d| d.siege.as_ref()).map_or(0, |s| s.crew) as usize;
        for u in self.units.iter_mut().filter(|u| builders.contains(&u.id)).take(crew) {
            u.crew_of = Some(uid);
        }
        self.dirty_stocks.insert(b.owner);
    }

    /// Freie Baumeister neben einem Gerät, dem Bedienung fehlt, übernehmen sie.
    fn assign_crew(&mut self) {
        let mut counts = self.crew_counts();
        let devices: Vec<usize> = (0..self.units.len()).filter(|&k| self.units[k].kind.is_siege()).collect();
        for d in devices {
            let (did, owner, pos) = (self.units[d].id, self.units[d].owner, self.units[d].pos);
            let need = self.economy.unit(self.units[d].kind).and_then(|x| x.siege.as_ref()).map_or(0, |s| s.crew);
            let mut have = counts.get(&did).copied().unwrap_or(0);
            if have >= need {
                continue;
            }
            for k in 0..self.units.len() {
                if have >= need {
                    break;
                }
                let u = &self.units[k];
                if u.owner != owner || u.kind != UnitKind::Builder || u.crew_of.is_some() || !u.path.is_empty() || u.down > 0.0 {
                    continue;
                }
                if (u.pos.0 - pos.0).hypot(u.pos.1 - pos.1) > CREW_REACH {
                    continue;
                }
                // Wer an einer Baustelle arbeitet, bleibt dort.
                if self.buildings.iter().any(|b| b.owner == owner && b.site.is_some() && dist_to(b, u.pos) <= SITE_REACH) {
                    continue;
                }
                self.units[k].crew_of = Some(did);
                have += 1;
            }
            counts.insert(did, have);
        }
    }

    /// Wie viel Nachschub braucht dieses Gerät, bis es wieder voll ist? (Ware, Menge)
    pub(super) fn ammo_need(&self, u: &Unit) -> Option<(protocol::Good, u32)> {
        let sd = self.economy.unit(u.kind)?.siege.as_ref()?;
        let per = sd.ammo.filter(|a| a.amount > 0 && sd.shots > 0)?;
        let need = (u.shots_fired * per.amount).saturating_sub(u.ammo_part);
        (need > 0).then_some((per.good, need))
    }

    /// Nachschub anliefern; liefert, wie viel davon gebraucht wurde.
    pub(super) fn load_ammo(&mut self, k: usize, load: GoodAmount) -> u32 {
        let Some((good, need)) = self.ammo_need(&self.units[k]) else { return 0 };
        if good != load.good {
            return 0;
        }
        let per = self.economy.unit(self.units[k].kind).and_then(|d| d.siege.as_ref()).and_then(|s| s.ammo).map_or(1, |a| a.amount.max(1));
        let used = load.amount.min(need);
        let u = &mut self.units[k];
        u.ammo_part += used;
        let shots = (u.ammo_part / per).min(u.shots_fired);
        u.shots_fired -= shots;
        u.ammo_part -= shots * per;
        if u.shots_fired == 0 {
            u.ammo_part = 0;
        }
        used
    }

    /// Fährt ein Belagerungsturm an diese Mauer heran, oder liegt er schon an?
    pub fn order_dock(&mut self, player: u32, unit_ids: &[u32], building: u32) -> bool {
        let Some(b) = self.buildings.iter().find(|b| b.id == building && b.hp > 0) else { return false };
        if !self.dockable(b, player) {
            return false;
        }
        let mut any = false;
        for u in self.units.iter_mut().filter(|u| u.owner == player && unit_ids.contains(&u.id)) {
            if !self.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).is_some_and(|s| s.docks) {
                continue;
            }
            u.dock_target = Some(building);
            u.dock = None;
            u.path.clear();
            u.layers.clear();
            u.chase_timer = 0.0;
            u.attack_order = None;
            any = true;
        }
        any
    }

    /// Fremde Mauer (Gegner) mit Wehrgang: dort kann ein Turm andocken.
    fn dockable(&self, b: &BuildingState, player: u32) -> bool {
        b.hp > 0 && self.can_attack(player, b.owner) && self.economy.building(b.kind).is_some_and(|d| d.at(b.level).walkway)
    }

    /// Feld des Wehrgangs von `b` direkt neben diesem Bodenfeld (Höhe passt), falls es eins gibt.
    fn dock_tile(&self, b: &BuildingState, (x, y): (i64, i64)) -> Option<usize> {
        let w = self.map.width as i64;
        let here = self.tile_of((x as f32 + 0.5, y as f32 + 0.5))?;
        let step = self.economy.def.movement.walkway_step_levels;
        [(1, 0), (-1, 0), (0, 1), (0, -1)].into_iter().find_map(|(dx, dy)| {
            let (nx, ny) = (x + dx, y + dy);
            if nx < b.x as i64 || ny < b.y as i64 || nx >= (b.x + b.w) as i64 || ny >= (b.y + b.h) as i64 {
                return None;
            }
            let n = (ny * w + nx) as usize;
            (self.map.levels[n].abs_diff(self.map.levels[here]) <= step).then_some(n)
        })
    }

    /// Belagerungstürme an ihre Mauer fahren und andocken lassen; merkt sich, welche Wehrgänge offen sind.
    fn step_towers(&mut self, dt: f32) {
        let mut storms = Vec::new();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if !u.kind.is_siege() || (u.dock_target.is_none() && u.dock.is_none()) {
                continue;
            }
            let target = u.dock.map(|d| d.0).or(u.dock_target);
            let b = target.and_then(|id| self.buildings.iter().find(|b| b.id == id)).filter(|b| self.dockable(b, u.owner)).cloned();
            let Some(b) = b else {
                let u = &mut self.units[k];
                u.dock = None;
                u.dock_target = None;
                continue;
            };
            let tile = (u.pos.0.floor() as i64, u.pos.1.floor() as i64);
            if let Some((_, top)) = u.dock {
                storms.push((b.owner, u.owner, top, tile));
                continue;
            }
            // Angekommen: an der Mauer andocken.
            if u.path.is_empty() {
                if let Some(top) = self.dock_tile(&b, tile) {
                    let (owner, wall) = (u.owner, b.owner);
                    let u = &mut self.units[k];
                    u.dock = Some((b.id, top));
                    u.dock_target = None;
                    u.target = None;
                    storms.push((wall, owner, top, tile));
                    continue;
                }
            }
            let u = &mut self.units[k];
            u.chase_timer = (u.chase_timer - dt).max(0.0);
            if !u.path.is_empty() || u.chase_timer > 0.0 {
                continue;
            }
            u.chase_timer = 2.0;
            // Freie Felder direkt vor der Mauer, das nächste, das man erreicht.
            let (from, owner) = (u.pos, u.owner);
            let (x0, y0, x1, y1) = (b.x as i64, b.y as i64, (b.x + b.w) as i64, (b.y + b.h) as i64);
            let mut spots: Vec<(i64, i64)> = (x0..x1)
                .flat_map(|x| [(x, y0 - 1), (x, y1)])
                .chain((y0..y1).flat_map(|y| [(x0 - 1, y), (x1, y)]))
                .filter(|&(x, y)| self.map.nav.is_open(x as i32, y as i32) && self.dock_tile(&b, (x, y)).is_some())
                .collect();
            let d2 = |p: &(i64, i64)| (p.0 as f32 + 0.5 - from.0).powi(2) + (p.1 as f32 + 0.5 - from.1).powi(2);
            spots.sort_by(|a, b| d2(a).total_cmp(&d2(b)));
            for spot in spots.into_iter().take(6) {
                let goal = (spot.0 as f32 + 0.5, spot.1 as f32 + 0.5);
                let (pts, layers) = self.plan(owner, from, false, goal, false);
                let end = pts.last().copied().unwrap_or(from);
                if (end.0.floor() as i64, end.1.floor() as i64) == spot {
                    let u = &mut self.units[k];
                    u.path = pts;
                    u.layers = layers;
                    u.target = Some(goal);
                    break;
                }
            }
        }
        // Ein Turm ist weg oder abgefahren: wer oben auf den fremden Mauern steht, muss herunter.
        let lost = self.storms.iter().any(|s| !storms.contains(s));
        self.storms = storms;
        if lost {
            self.drop_from_walls();
        }
    }

    /// Die Bedienung steht fest am Gerät und fährt mit (sie läuft nicht selbst).
    fn crew_follow(&mut self, _dt: f32) {
        let devices: HashMap<u32, ((f32, f32), (f32, f32), bool, f32)> = self
            .units
            .iter()
            .filter(|u| u.kind.is_siege())
            .map(|u| {
                let size = self.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).map_or(1, |s| if s.on_towers { 1 } else { s.size.max(1) });
                let r = if u.perch.is_some() { 0.6 } else { size as f32 * 0.5 + 0.35 };
                (u.id, (u.pos, u.vel, u.on_wall, r))
            })
            .collect();
        let mut slot: HashMap<u32, usize> = HashMap::new();
        for k in 0..self.units.len() {
            let Some(d) = self.units[k].crew_of else { continue };
            let Some(&(p, vel, wall, r)) = devices.get(&d) else { continue };
            let i = slot.entry(d).or_insert(0);
            // Hinter dem Gerät aufgereiht (gegen die Fahrtrichtung, sonst nach Süden).
            let speed = vel.0.hypot(vel.1);
            let back = if speed > 1e-3 { (-vel.1).atan2(-vel.0) } else { std::f32::consts::FRAC_PI_2 };
            let a = back + (*i as f32 - 1.0) * 0.7;
            *i += 1;
            let (w, h) = (self.map.width as f32 - 0.01, self.map.height as f32 - 0.01);
            let spot = ((p.0 + a.cos() * r).clamp(0.0, w), (p.1 + a.sin() * r).clamp(0.0, h));
            let u = &mut self.units[k];
            u.pos = spot;
            u.vel = vel;
            u.on_wall = wall;
            u.path.clear();
            u.layers.clear();
            u.target = None;
        }
    }

    /// Die Bedienung dieser eigenen Geräte steigt ab und tritt ein Stück zur Seite.
    pub fn release_crew(&mut self, player: u32, device_ids: &[u32]) -> bool {
        let mut any = false;
        for &did in device_ids {
            let Some(dev) = self.units.iter().find(|u| u.id == did && u.owner == player && u.kind.is_siege()) else { continue };
            let (pos, wall, perch) = (dev.pos, dev.on_wall, dev.perch);
            let crew: Vec<usize> = (0..self.units.len()).filter(|&k| self.units[k].crew_of == Some(did)).collect();
            // Auf dem Turm: zum Fuß des Turms, sonst neben das Gerät.
            let base = perch.and_then(|t| self.buildings.iter().find(|b| b.id == t)).map(|b| center(b)).unwrap_or(pos);
            for (n, k) in crew.into_iter().enumerate() {
                let a = n as f32 * 1.3;
                let spot = (base.0 + a.cos() * (CREW_REACH + 1.0), base.1 + a.sin() * (CREW_REACH + 1.0));
                let (owner, from) = (self.units[k].owner, self.units[k].pos);
                let (path, layers) = self.plan(owner, from, wall, spot, false);
                let u = &mut self.units[k];
                u.crew_of = None;
                u.path = path;
                u.layers = layers;
                u.target = Some(spot);
                u.target_wall = false;
                any = true;
            }
        }
        any
    }

    /// Zufallszahl 0..1 (Streuung der Belagerungsgeräte).
    pub(super) fn rand01(&mut self) -> f32 {
        let mut x = self.combat_rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.combat_rng = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / (1u64 << 24) as f32
    }

    /// Ist dieses Gebäude ein Ziel für Belagerungsgeräte (nicht Wege, nicht Ruinen)?
    fn siege_target(&self, b: &BuildingState) -> bool {
        b.hp > 0 && self.economy.building(b.kind).is_some_and(|d| !d.is_road())
    }

    /// Belagerungsgeräte zielen und schießen (Teil von `step_combat`). Liefert die Schüsse.
    pub(super) fn siege_fire(&mut self, dt: f32, grid: &HashMap<(i32, i32), Vec<usize>>, cell: f32) -> Vec<Shot> {
        let devices: Vec<usize> = (0..self.units.len()).filter(|&k| self.units[k].kind.is_siege()).collect();
        if devices.is_empty() {
            return Vec::new();
        }
        let counts = self.crew_counts();
        let index: HashMap<u32, usize> = self.units.iter().enumerate().map(|(k, u)| (u.id, k)).collect();
        let mut shots = Vec::new();
        for k in devices {
            let u = &self.units[k];
            let Some(def) = self.economy.unit(u.kind).cloned() else { continue };
            let def = &def;
            let Some(sd) = def.siege.clone() else { continue };
            if def.range <= 0.0 || !self.crewed(u, &counts) || u.down > 0.0 {
                continue;
            }
            // Vorrat leer: wartet auf Nachschub.
            if sd.shots > 0 && u.shots_fired >= sd.shots {
                continue;
            }
            let range = def.range;
            // Ziel: Befehl, sonst von selbst (Einheiten zuerst, dann Gebäude).
            enum Aim {
                Unit(usize),
                Building(u32),
            }
            let mut aim: Option<Aim> = None;
            let mut chase: Option<(f32, f32)> = None;
            if let Some(order) = u.attack_order {
                match order {
                    AttackTarget::Unit { id } => match index.get(&id) {
                        Some(&j) if sd.hits_units && (u.attack_force || self.can_attack(u.owner, self.units[j].owner)) => {
                            let t = &self.units[j];
                            let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                            if d <= self.range_against(u, t, range) && d >= sd.min_range {
                                aim = Some(Aim::Unit(j));
                            } else if d > sd.min_range {
                                chase = Some(t.pos);
                            }
                        }
                        _ => self.units[k].attack_order = None,
                    },
                    AttackTarget::Building { id } => match self.buildings.iter().find(|b| b.id == id && self.siege_target(b)) {
                        Some(b) if sd.hits_buildings && (u.attack_force || self.can_attack(u.owner, b.owner)) => {
                            let d = dist_to(b, u.pos);
                            if d <= range && d >= sd.min_range {
                                aim = Some(Aim::Building(id));
                            } else if d > sd.min_range {
                                chase = Some(center(b));
                            }
                        }
                        _ => self.units[k].attack_order = None,
                    },
                }
            }
            let u = &self.units[k];
            if u.attack_order.is_none() && !u.path.is_empty() {
                continue;
            }
            if u.attack_order.is_none() {
                if sd.hits_units && sd.auto_units {
                    let reach = range * 2.0;
                    let (cx, cy) = ((u.pos.0 / cell) as i32, (u.pos.1 / cell) as i32);
                    let r = (reach / cell).ceil() as i32;
                    let mut best: Option<((bool, f32), usize)> = None;
                    for gy in cy - r..=cy + r {
                        for gx in cx - r..=cx + r {
                            for &j in grid.get(&(gx, gy)).map_or(&[][..], |v| v.as_slice()) {
                                let t = &self.units[j];
                                if t.owner == u.owner || !self.can_attack(u.owner, t.owner) || !self.can_damage(def, t) {
                                    continue;
                                }
                                if t.kind.is_civilian() && self.inside(t) {
                                    continue;
                                }
                                let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                                if d < sd.min_range || d > self.range_against(u, t, range) {
                                    continue;
                                }
                                let civilian = t.kind.is_civilian() || self.economy.unit(t.kind).is_none_or(|x| x.attack == 0);
                                let key = (civilian, d);
                                if best.is_none_or(|(b, _)| key.0 < b.0 || (key.0 == b.0 && key.1 < b.1)) {
                                    best = Some((key, j));
                                }
                            }
                        }
                    }
                    aim = best.map(|(_, j)| Aim::Unit(j));
                }
                if aim.is_none() && sd.hits_buildings {
                    aim = self
                        .buildings
                        .iter()
                        .filter(|b| b.owner != u.owner && self.siege_target(b) && self.can_attack(u.owner, b.owner))
                        .map(|b| (dist_to(b, u.pos), b.id))
                        .filter(|&(d, _)| d <= range && d >= sd.min_range)
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, id)| Aim::Building(id));
                }
            }
            // Außer Reichweite: wer fahren kann, rollt heran.
            if aim.is_none() {
                if let Some(to) = chase {
                    let mobile = !self.immobile(&self.units[k]);
                    let u = &mut self.units[k];
                    u.chase_timer = (u.chase_timer - dt).max(0.0);
                    if mobile && (u.path.is_empty() || u.chase_timer <= 0.0) {
                        u.chase_timer = 2.0;
                        let (owner, from) = (u.owner, u.pos);
                        let (path, layers) = self.plan(owner, from, false, to, false);
                        let u = &mut self.units[k];
                        u.path = path;
                        u.layers = layers;
                        u.target = Some(to);
                    }
                }
                continue;
            }
            // Zum Schießen anhalten.
            {
                let u = &mut self.units[k];
                if !u.path.is_empty() {
                    u.path.clear();
                    u.layers.clear();
                    u.target = Some(u.pos);
                }
                if u.attack_cooldown > 0.0 {
                    continue;
                }
            }
            let u = &self.units[k];
            let speed = sd.shot_speed.max(1.0);
            let (target_pos, target_wall, foe) = match aim {
                Some(Aim::Unit(j)) => {
                    let t = &self.units[j];
                    let d = (t.pos.0 - u.pos.0).hypot(t.pos.1 - u.pos.1);
                    let secs = d / speed;
                    // Vorhalten wie die Schützen.
                    let p = (t.pos.0 + t.vel.0 * secs, t.pos.1 + t.vel.1 * secs);
                    (p, t.on_wall, Some(j))
                }
                Some(Aim::Building(id)) => {
                    let b = self.buildings.iter().find(|b| b.id == id).expect("eben gefunden");
                    (center(b), false, None)
                }
                None => continue,
            };
            let d = (target_pos.0 - u.pos.0).hypot(target_pos.1 - u.pos.1);
            let spread = def.scatter * (d / range).min(1.0);
            let (a, r) = (self.rand01() * std::f32::consts::TAU, self.rand01().sqrt() * spread);
            let (mw, mh) = (self.map.width as f32 - 0.01, self.map.height as f32 - 0.01);
            let to = ((target_pos.0 + a.cos() * r).clamp(0.0, mw), (target_pos.1 + a.sin() * r).clamp(0.0, mh));
            let u = &self.units[k];
            let d = (to.0 - u.pos.0).hypot(to.1 - u.pos.1);
            let seconds = (d / speed).max(0.3);
            let from_h = self.feet_h(u) + 0.9;
            let mut ghost = Unit::new(0, 0, UnitKind::Archer, to);
            ghost.on_wall = target_wall && self.tile_of(to).is_some_and(|i| self.occupied[i] != 0);
            let mut to_h = self.feet_h(&ghost) + 0.5;
            if let Some(b) = self.tile_of(to).map(|i| self.occupied[i]).and_then(|id| self.buildings.iter().find(|b| b.id == id)) {
                if !ghost.on_wall {
                    to_h = to_h.max(self.feet_h(&ghost) + 0.6 * self.building_height(b));
                }
            }
            // Wurfbahn: Katapult und Tribok werfen steil, die Balliste flach.
            let steep = sd.hits_buildings;
            let reach = range.max(d + 0.1);
            let aa = d * d / (2.0 * reach);
            let disc = d * d - 4.0 * aa * (to_h - from_h + aa);
            let arc = (d > 0.5 && disc >= 0.0).then(|| {
                let root = disc.sqrt();
                let t = if steep { (d + root) / (2.0 * aa) } else { (d - root) / (2.0 * aa) };
                (t, reach)
            });
            shots.push(Shot { from: u.pos, to, from_wall: u.on_wall, to_wall: ghost.on_wall, kind: u.kind, from_h, to_h, arc, seconds });
            self.siege_shots.push(SiegeShot { shooter: u.id, owner: u.owner, kind: u.kind, to, left: seconds });
            let shooter = u.id;
            let u = &mut self.units[k];
            u.attack_cooldown = def.attack_seconds;
            if sd.shots > 0 {
                u.shots_fired += 1;
            }
            u.strikes = u.strikes.wrapping_add(1);
            if let Some(j) = foe {
                let fid = self.units[j].id;
                self.units[k].last_foe = Some((fid, 0.0));
                self.units[j].attacked_by = Some((shooter, 0.0));
            }
        }
        shots
    }

    /// Steine und Bolzen, die jetzt einschlagen: (Treffer auf Einheiten, Treffer auf Gebäude).
    pub(super) fn land_siege(&mut self, dt: f32, grid: &HashMap<(i32, i32), Vec<usize>>, cell: f32) -> (Vec<super::combat::Hit>, Vec<(u32, UnitKind)>) {
        if self.siege_shots.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut arrived = Vec::new();
        self.siege_shots.retain_mut(|s| {
            s.left -= dt;
            if s.left > 0.0 {
                return true;
            }
            arrived.push(s.clone());
            false
        });
        let hit_radius = self.economy.def.army.arrow_hit_radius;
        let mut hits = Vec::new();
        let mut building_hits = Vec::new();
        for s in arrived {
            let Some(def) = self.economy.unit(s.kind) else { continue };
            let Some(sd) = def.siege.as_ref() else { continue };
            if sd.hits_buildings {
                let id = self.tile_of(s.to).map_or(0, |i| self.occupied[i]);
                if let Some(b) = self.buildings.iter().find(|b| b.id == id) {
                    if b.owner != s.owner && self.can_attack(s.owner, b.owner) && self.siege_target(b) {
                        building_hits.push((id, s.kind));
                    }
                }
            }
            if sd.hits_units {
                let radius = if sd.splash > 0.0 { sd.splash } else { hit_radius };
                let (cx, cy) = ((s.to.0 / cell) as i32, (s.to.1 / cell) as i32);
                let mut near: Vec<(f32, usize)> = Vec::new();
                for gy in cy - 1..=cy + 1 {
                    for gx in cx - 1..=cx + 1 {
                        for &j in grid.get(&(gx, gy)).map_or(&[][..], |v| v.as_slice()) {
                            let Some(t) = self.units.get(j) else { continue };
                            if t.owner == s.owner || !self.can_attack(s.owner, t.owner) || !self.can_damage(def, t) {
                                continue;
                            }
                            if t.kind.is_civilian() && self.inside(t) {
                                continue;
                            }
                            let d = (t.pos.0 - s.to.0).hypot(t.pos.1 - s.to.1);
                            if d <= radius {
                                near.push((d, j));
                            }
                        }
                    }
                }
                near.sort_by(|a, b| a.0.total_cmp(&b.0));
                if sd.splash <= 0.0 {
                    near.truncate(1);
                }
                for (_, j) in near {
                    hits.push((j, s.shooter, s.kind, s.owner, true));
                }
            }
        }
        (hits, building_hits)
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    /// Flache Karte, zwei Spieler ohne Bewohner, Belagerungswerkstatt Stufe 2 für Spieler 1.
    fn world() -> World {
        let row = "0".repeat(120);
        let text = format!("spawn 10 10\nspawn 100 10\n{}", format!("{row}\n").repeat(60));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_build_radius();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.spawn_player(2);
        let goods: Vec<GoodAmount> = [Good::Wood, Good::Boards, Good::Stone, Good::Blocks, Good::Iron]
            .into_iter()
            .filter(|g| w.economy.def.goods.contains(g))
            .map(|good| GoodAmount { good, amount: 500 })
            .collect();
        w.admin_give(1, &goods, 5000);
        w.admin_give(2, &goods, 5000);
        let ws = w.place_building(1, BuildingKind::SiegeWorkshop, 20, 20).unwrap().id;
        w.upgrade(1, ws).unwrap();
        w.take_events();
        w
    }

    fn builder(w: &mut World, id: u32, owner: u32, pos: (f32, f32)) {
        w.units.push(Unit::new(id, owner, UnitKind::Builder, pos));
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds / 0.05) as u32 {
            w.step(0.05);
        }
    }

    fn device(w: &World, kind: UnitKind) -> Option<&Unit> {
        w.units.iter().find(|u| u.kind == kind)
    }

    #[test]
    fn baumeister_bauen_mit_rucksack_ein_katapult() {
        let mut w = world();
        // Kosten so klein, dass die Rucksäcke reichen.
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Catapult).unwrap();
        d.cost = vec![GoodAmount { good: Good::Boards, amount: 8 }];
        d.seconds = 6.0;
        builder(&mut w, 9001, 1, (40.5, 40.5));
        builder(&mut w, 9002, 1, (41.5, 40.5));
        let boards = w.stock_state(1).goods.iter().find(|g| g.good == Good::Boards).map_or(0, |g| g.amount);
        let site = w.build_siege(1, &[9001, 9002], UnitKind::Catapult, (45, 40), true).unwrap();
        assert_eq!(site.kind, BuildingKind::SiegeSite);
        let after = w.stock_state(1).goods.iter().find(|g| g.good == Good::Boards).map_or(0, |g| g.amount);
        assert_eq!(boards - after, 8, "zwei Rucksäcke: 5 + 3 Bretter");
        run(&mut w, 15.0);
        assert!(!w.buildings.iter().any(|b| b.kind == BuildingKind::SiegeSite), "Baustelle ist fertig");
        let cat = device(&w, UnitKind::Catapult).expect("Katapult steht").id;
        let crew = w.units.iter().filter(|u| u.crew_of == Some(cat)).count();
        assert_eq!(crew, 2, "beide Baumeister bedienen es");
    }

    #[test]
    fn ohne_baustoffe_kein_fortschritt_ueber_den_anteil() {
        let mut w = world();
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Catapult).unwrap();
        d.cost = vec![GoodAmount { good: Good::Boards, amount: 20 }];
        d.seconds = 4.0;
        builder(&mut w, 9001, 1, (40.5, 40.5));
        let site = w.build_siege(1, &[9001], UnitKind::Catapult, (45, 40), true).unwrap().id;
        run(&mut w, 20.0);
        let b = w.buildings.iter().find(|b| b.id == site).expect("noch im Bau");
        let s = b.site.as_ref().unwrap();
        assert!((s.work - 1.0).abs() < 0.1, "5 von 20 Brettern = ein Viertel der Arbeit: {}", s.work);
    }

    #[test]
    fn karren_bringen_baustoffe_auch_weit_weg() {
        let mut w = world();
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Catapult).unwrap();
        d.cost = vec![GoodAmount { good: Good::Boards, amount: 30 }];
        d.seconds = 3.0;
        w.place_building_rotated(1, BuildingKind::Storehouse, 14, 30, 0, 1).unwrap();
        let shed = w.place_building_rotated(1, BuildingKind::CartShed, 26, 30, 0, 1).unwrap().id;
        let _ = w.upgrade(1, shed);
        run(&mut w, 2.0);
        assert!(w.units.iter().any(|u| u.kind == UnitKind::Cart), "Karren da");
        builder(&mut w, 9001, 1, (85.5, 50.5));
        let site = w.build_siege(1, &[9001], UnitKind::Catapult, (88, 50), false).unwrap().id;
        let mut done = false;
        for _ in 0..240 {
            run(&mut w, 1.0);
            if !w.buildings.iter().any(|b| b.id == site) {
                done = true;
                break;
            }
        }
        assert!(done, "Karren haben geliefert, Katapult fertig");
    }

    #[test]
    fn ohne_werkstatt_kein_bau() {
        let mut w = world();
        builder(&mut w, 9001, 2, (80.5, 40.5));
        assert!(w.build_siege(2, &[9001], UnitKind::Catapult, (82, 40), false).is_err());
        // Tribok erst ab Stufe 2 – Spieler 1 hat sie.
        builder(&mut w, 9002, 1, (40.5, 40.5));
        assert!(w.build_siege(1, &[9002], UnitKind::Trebuchet, (44, 40), false).is_ok());
    }

    #[test]
    fn tribok_wirft_auf_mauern_und_braucht_bedienung() {
        let mut w = world();
        let wall = w.place_building(2, BuildingKind::Wall, 80, 50).unwrap().id;
        let hp0 = w.buildings.iter().find(|b| b.id == wall).unwrap().hp;
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Trebuchet).unwrap();
        d.scatter = 0.0;
        d.attack_seconds = 2.0;
        let range = d.range;
        let x = 80.5 - range + 3.0;
        let mut t = Unit::new(9100, 1, UnitKind::Trebuchet, (x, 50.5));
        t.attack_cooldown = 0.0;
        w.units.push(t);
        run(&mut w, 8.0);
        assert_eq!(w.buildings.iter().find(|b| b.id == wall).unwrap().hp, hp0, "ohne Bedienung kein Wurf");
        for i in 0..3 {
            builder(&mut w, 9200 + i, 1, (x - 1.0, 50.5 + i as f32 * 0.6));
        }
        run(&mut w, 12.0);
        let hp = w.buildings.iter().find(|b| b.id == wall).map_or(0, |b| b.hp);
        assert!(hp < hp0, "Mauer getroffen: {hp} von {hp0}");
    }

    #[test]
    fn balliste_entsteht_auf_dem_turm_und_schiesst_auf_einheiten() {
        let mut w = world();
        let tower = w.place_building(1, BuildingKind::Tower, 50, 20).unwrap().id;
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Ballista).unwrap();
        d.cost = vec![GoodAmount { good: Good::Boards, amount: 3 }];
        d.seconds = 2.0;
        builder(&mut w, 9001, 1, (49.5, 23.5));
        assert!(w.build_siege(1, &[9001], UnitKind::Ballista, (45, 40), true).is_err(), "nur auf Türmen");
        w.build_siege(1, &[9001], UnitKind::Ballista, (51, 21), true).unwrap();
        run(&mut w, 8.0);
        let b = device(&w, UnitKind::Ballista).expect("Balliste steht").clone();
        assert_eq!(b.perch, Some(tower));
        assert!(b.on_wall);
        assert!(w.build_siege(1, &[9001], UnitKind::Ballista, (51, 21), false).is_err(), "eine je Turm");
        let mut foe = Unit::new(9500, 2, UnitKind::Swordsman, (51.5, 40.5));
        foe.attack_order = None;
        w.units.push(foe);
        run(&mut w, 10.0);
        let hurt = w.units.iter().find(|u| u.id == 9500).is_none_or(|u| u.damage > 0);
        assert!(hurt, "Bolzen treffen den Schwertkämpfer");
        // Turm weg: Balliste auch.
        w.demolish(1, tower).unwrap();
        run(&mut w, 0.2);
        assert!(device(&w, UnitKind::Ballista).is_none());
    }

    #[test]
    fn katapult_faehrt_nur_mit_bedienung() {
        let mut w = world();
        w.units.push(Unit::new(9100, 1, UnitKind::Catapult, (40.5, 40.5)));
        w.order_move(1, &[9100], (50.5, 40.5));
        run(&mut w, 3.0);
        assert!((device(&w, UnitKind::Catapult).unwrap().pos.0 - 40.5).abs() < 0.01, "steht ohne Bedienung");
        builder(&mut w, 9001, 1, (39.5, 40.5));
        builder(&mut w, 9002, 1, (39.5, 41.5));
        run(&mut w, 1.0);
        w.order_move(1, &[9100], (50.5, 40.5));
        run(&mut w, 30.0);
        let c = device(&w, UnitKind::Catapult).unwrap().pos;
        assert!(c.0 > 48.0, "gerollt: {c:?}");
        let crew_near = w.units.iter().filter(|u| u.kind == UnitKind::Builder && (u.pos.0 - c.0).hypot(u.pos.1 - c.1) < 3.5).count();
        assert_eq!(crew_near, 2, "Bedienung ist mitgelaufen");
    }

    #[test]
    fn bedienung_faehrt_fest_mit_und_steigt_auf_befehl_ab() {
        let mut w = world();
        w.units.push(Unit::new(9100, 1, UnitKind::Catapult, (40.5, 40.5)));
        builder(&mut w, 9001, 1, (39.5, 40.5));
        builder(&mut w, 9002, 1, (39.5, 41.5));
        run(&mut w, 0.5);
        w.order_move(1, &[9100], (50.5, 40.5));
        for _ in 0..200 {
            w.step(0.05);
            let c = device(&w, UnitKind::Catapult).unwrap().pos;
            for u in w.units.iter().filter(|u| u.kind == UnitKind::Builder) {
                assert_eq!(u.crew_of, Some(9100));
                assert!((u.pos.0 - c.0).hypot(u.pos.1 - c.1) < 2.0, "dicht am Gerät");
            }
        }
        assert!(w.release_crew(1, &[9100]));
        assert!(!w.release_crew(2, &[9100]), "nur eigene Geräte");
        run(&mut w, 6.0);
        assert!(w.units.iter().filter(|u| u.kind == UnitKind::Builder).all(|u| u.crew_of.is_none()), "steigt ab und bleibt frei");
    }

    #[test]
    fn katapult_greift_einheiten_nur_auf_befehl_an() {
        let mut w = world();
        w.units.push(Unit::new(9100, 1, UnitKind::Catapult, (40.5, 40.5)));
        builder(&mut w, 9001, 1, (39.5, 40.5));
        builder(&mut w, 9002, 1, (39.5, 41.5));
        let mut foe = Unit::new(9500, 2, UnitKind::Spearman, (55.5, 40.5));
        foe.attack_order = None;
        w.units.push(foe);
        run(&mut w, 15.0);
        assert!(w.siege_shots.is_empty() && w.units.iter().find(|u| u.id == 9500).is_some_and(|u| u.damage == 0), "kein Schuss von selbst");
        assert!(w.order_attack(1, &[9100], protocol::AttackTarget::Unit { id: 9500 }));
        let mut shot = false;
        for _ in 0..300 {
            w.step(0.05);
            shot |= !w.siege_shots.is_empty();
        }
        assert!(shot, "mit Befehl wirft es");
    }

    #[test]
    fn streuung_verteilt_die_einschlaege() {
        let mut w = world();
        w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Archer).unwrap().scatter = 3.0;
        w.economy.def.army.arrow_hit_radius = 0.1;
        let mut archer = Unit::new(9200, 1, UnitKind::Archer, (40.5, 30.5));
        archer.attack_order = None;
        w.units.push(archer);
        let mut foe = Unit::new(9500, 2, UnitKind::Knight, (58.5, 30.5));
        foe.attack_order = None;
        w.units.push(foe);
        let mut spots = Vec::new();
        for _ in 0..600 {
            w.step(0.05);
            for e in w.take_events() {
                if let protocol::ServerMsg::Shots { shots } = e {
                    spots.extend(shots.iter().filter(|s| s.kind == UnitKind::Archer).map(|s| s.to));
                }
            }
            if let Some(f) = w.units.iter_mut().find(|u| u.id == 9500) {
                f.pos = (58.5, 30.5);
                f.damage = 0;
            }
        }
        assert!(spots.len() >= 5, "geschossen: {}", spots.len());
        let off = spots.iter().filter(|p| (p.0 - 58.5).hypot(p.1 - 30.5) > 0.3).count();
        assert!(off * 2 > spots.len(), "die meisten streuen: {off} von {}", spots.len());
        assert!(spots.iter().all(|p| (p.0 - 58.5).hypot(p.1 - 30.5) <= 3.01), "nicht weiter als der Radius");
    }

    #[test]
    fn vorrat_leer_dann_bringen_karren_nachschub() {
        let mut w = world();
        let d = w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Catapult).unwrap();
        d.attack_seconds = 0.5;
        let sd = d.siege.as_mut().unwrap();
        sd.shots = 3;
        sd.ammo = Some(GoodAmount { good: Good::Stone, amount: 2 });
        // Mauer des Gegners als Ziel, weit weg vom eigenen Lager.
        let wall = w.place_building_rotated(2, BuildingKind::Wall, 80, 50, 0, 1).unwrap().id;
        w.buildings.iter_mut().find(|b| b.id == wall).unwrap().hp = 100_000;
        w.units.push(Unit::new(9100, 1, UnitKind::Catapult, (60.5, 50.5)));
        builder(&mut w, 9001, 1, (59.5, 50.5));
        builder(&mut w, 9002, 1, (59.5, 51.5));
        let mut fired = 0;
        for _ in 0..400 {
            w.step(0.05);
            for e in w.take_events() {
                if let protocol::ServerMsg::Shots { shots } = e {
                    fired += shots.iter().filter(|s| s.kind == UnitKind::Catapult).count();
                }
            }
        }
        assert_eq!(fired, 3, "nur so viele Schüsse, wie im Vorrat sind");
        let cat = w.units.iter().find(|u| u.id == 9100).unwrap();
        assert_eq!(w.snapshot_unit_shots(cat), Some(0));
        // Jetzt Lager und Karren: Nachschub kommt, es schießt wieder.
        let stone = w.stocks[&1].amount(Good::Stone);
        w.place_building_rotated(1, BuildingKind::Storehouse, 14, 30, 0, 1).unwrap();
        let shed = w.place_building_rotated(1, BuildingKind::CartShed, 26, 30, 0, 1).unwrap().id;
        let _ = w.upgrade(1, shed);
        let mut again = false;
        for _ in 0..240 {
            run(&mut w, 1.0);
            for e in w.take_events() {
                if let protocol::ServerMsg::Shots { shots } = e {
                    again |= shots.iter().any(|s| s.kind == UnitKind::Catapult);
                }
            }
            if again {
                break;
            }
        }
        assert!(again, "nach dem Nachschub schießt es wieder");
        assert!(w.stocks[&1].amount(Good::Stone) < stone, "Steine kamen aus dem Vorrat");
    }

    #[test]
    fn belagerungsturm_dockt_an_und_truppen_steigen_auf_die_mauer() {
        let mut w = world();
        for x in 0..120 {
            w.place_building_rotated(2, BuildingKind::Wall, x, 45, 0, 2).unwrap();
        }
        let wall = w.buildings.iter().find(|b| b.kind == BuildingKind::Wall && b.x == 62).unwrap().id;
        w.units.push(Unit::new(9100, 1, UnitKind::SiegeTower, (60.5, 35.5)));
        builder(&mut w, 9001, 1, (59.5, 35.5));
        builder(&mut w, 9002, 1, (59.5, 36.5));
        w.units.push(Unit::new(9200, 1, UnitKind::Spearman, (55.5, 33.5)));
        run(&mut w, 0.5);
        assert!(!w.order_move(1, &[9200], (70.5, 45.5)) || !w.units.iter().find(|u| u.id == 9200).unwrap().layers.iter().any(|&l| l), "ohne Turm nicht hinauf");
        assert!(w.order_dock(1, &[9100], wall));
        assert!(!w.order_dock(2, &[9100], wall), "nur eigene Türme");
        let mut docked = false;
        for _ in 0..60 {
            run(&mut w, 1.0);
            if w.units.iter().find(|u| u.id == 9100).unwrap().dock.is_some() {
                docked = true;
                break;
            }
        }
        assert!(docked, "Turm liegt an: {:?}", w.units.iter().find(|u| u.id == 9100).unwrap().pos);
        assert!(w.order_move(1, &[9200], (70.5, 45.5)));
        run(&mut w, 40.0);
        let sp = w.units.iter().find(|u| u.id == 9200).unwrap();
        assert!(sp.on_wall, "oben auf der feindlichen Mauer: {:?}", sp.pos);
        assert!((sp.pos.0 - 70.5).abs() < 1.0 && (sp.pos.1 - 45.5).abs() < 1.0, "{:?}", sp.pos);
        // Turm fährt weg: Wer oben steht, muss herunter.
        w.order_move(1, &[9100], (40.5, 30.5));
        run(&mut w, 1.0);
        assert!(w.units.iter().find(|u| u.id == 9100).unwrap().dock.is_none());
        assert!(!w.units.iter().find(|u| u.id == 9200).unwrap().on_wall, "ohne Turm nicht mehr oben");
    }
}
