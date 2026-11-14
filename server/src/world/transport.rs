//! Transport: Träger und Karren bringen Waren zwischen Betrieben und Vorrat.
//!
//! Der gemeinsame Vorrat eines Spielers liegt in seinen Lagern (Bergfried,
//! Vorratslager). Jeder Betrieb hat ein kleines eigenes Lager. Zwei Aufträge:
//!
//! - **Abholen:** Fertige Waren (alles, was kein Eingang des gewählten Rezepts
//!   ist) aus dem Betrieb zum nächsten Lager bringen.
//! - **Beliefern:** Fehlende Eingänge aus dem Vorrat zum Betrieb bringen, aber
//!   nur, wenn der Betrieb Lieferungen annimmt (`accept_deliveries`). Er hält
//!   Eingänge für zwei Durchläufe vorrätig.
//!
//! Träger wohnen in den Lagern und tragen wenig. Der Karren aus dem
//! Wagenschuppen fährt erst ab `cart_min_load` los und lädt dann viel; er
//! lohnt sich für Betriebe, die viel herstellen oder weit weg liegen.
//! Was schon unterwegs ist, wird vorgemerkt, damit nicht zwei Träger dieselbe
//! Ladung holen.
//!
//! Bauen bezahlt man vorerst weiter direkt aus dem Vorrat.

use std::collections::HashMap;

use protocol::{BuildingState, Good, GoodAmount, UnitKind};

use super::World;

/// So lange wartet ein Träger, bevor er wieder nach Arbeit sucht.
const IDLE_WAIT: f32 = 1.0;
/// Nach einem gescheiterten Auftrag (Ziel weg, nicht erreichbar) etwas länger.
const FAIL_WAIT: f32 = 3.0;
/// So nah (Felder vom Gebäuderand) muss ein Träger kommen, um zu laden.
const REACH: f32 = 1.6;

/// Höchstens so viele Betriebe fährt ein Karren auf einer Sammelfahrt an.
const TOUR_STOPS: usize = 4;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Job {
    kind: JobKind,
    good: Good,
    amount: u32,
    stage: Stage,
    /// Sammelfahrt: weitere Betriebe (Id, Menge), die danach noch angefahren
    /// werden. Id 0 = leer.
    rest: [(u32, u32); TOUR_STOPS - 1],
}

/// Für Sammelfahrten: (Art, Ware) → Betriebe als (Abstand, Id, Menge).
type Groups = HashMap<(protocol::BuildingKind, Good), Vec<(f32, u32, u32)>>;

const NO_REST: [(u32, u32); TOUR_STOPS - 1] = [(0, 0); TOUR_STOPS - 1];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum JobKind {
    /// Ware aus dem Betrieb `from` in den Vorrat bringen.
    Pickup { from: u32 },
    /// Ware aus dem Vorrat zum Betrieb `to` bringen.
    Supply { to: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Stage {
    /// Unterwegs zum Aufladen bei diesem Gebäude.
    Load(u32),
    /// Unterwegs zum Abladen bei diesem Gebäude.
    Unload(u32),
}

/// Was schon vorgemerkt ist.
#[derive(Default)]
struct Reserved {
    /// (Betrieb, Ware) → wird abgeholt.
    pickup: HashMap<(u32, Good), u32>,
    /// (Betrieb, Ware) → ist zu ihm unterwegs.
    incoming: HashMap<(u32, Good), u32>,
    /// (Spieler, Ware) → wird gleich aus dem Vorrat genommen.
    central: HashMap<(u32, Good), u32>,
}

impl Reserved {
    fn add(&mut self, owner: u32, job: &Job) {
        match (job.kind, job.stage) {
            (JobKind::Pickup { from }, Stage::Load(_)) => {
                *self.pickup.entry((from, job.good)).or_default() += job.amount;
                for &(id, n) in job.rest.iter().filter(|r| r.0 != 0) {
                    *self.pickup.entry((id, job.good)).or_default() += n;
                }
            }
            (JobKind::Supply { to }, stage) => {
                *self.incoming.entry((to, job.good)).or_default() += job.amount;
                if matches!(stage, Stage::Load(_)) {
                    *self.central.entry((owner, job.good)).or_default() += job.amount;
                }
            }
            _ => {}
        }
    }

    fn get(map: &HashMap<(u32, Good), u32>, key: (u32, Good)) -> u32 {
        map.get(&key).copied().unwrap_or(0)
    }
}

/// Lagert dieses Lager die Ware? Ohne Auswahl lagert es alles.
fn stores(b: &BuildingState, good: Good) -> bool {
    b.settings.stores.as_ref().is_none_or(|s| s.contains(&good))
}

fn center(b: &BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

fn within(area: Option<((f32, f32), f32)>, b: &BuildingState) -> bool {
    area.is_none_or(|(c, r)| {
        let p = center(b);
        r <= 0.0 || ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() <= r
    })
}

/// Abstand eines Punktes zum Rechteck des Gebäudes (0 = darin).
fn dist_to(b: &BuildingState, p: (f32, f32)) -> f32 {
    let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
    let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

impl World {
    pub(super) fn step_transport(&mut self, dt: f32) {
        let index: HashMap<u32, usize> = self
            .buildings
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id, i))
            .collect();
        let mut reserved = Reserved::default();
        for u in &self.units {
            if let Some(job) = &u.job {
                reserved.add(u.owner, job);
            }
        }
        for k in 0..self.units.len() {
            if !self.units[k].is_transport() || !self.units[k].path.is_empty() {
                continue;
            }
            match self.units[k].job {
                Some(job) => self.arrive(k, job, &index),
                None => {
                    let u = &mut self.units[k];
                    u.cooldown -= dt;
                    if u.cooldown > 0.0 {
                        continue;
                    }
                    u.cooldown = IDLE_WAIT;
                    // Ohne Wohnung wird nicht gearbeitet.
                    let job = if self.idle_units.contains(&u.id) { None } else { self.find_job(k, &reserved, &index) };
                    if let Some(job) = job {
                        reserved.add(self.units[k].owner, &job);
                        self.start(k, job, &index);
                    } else {
                        self.go_home(k, &index);
                    }
                }
            }
        }
    }

    fn capacity(&self, kind: UnitKind) -> u32 {
        let t = &self.economy.def.transport;
        if kind == UnitKind::Cart {
            t.cart_capacity
        } else {
            t.carrier_capacity
        }
    }

    fn is_hub(&self, b: &BuildingState) -> bool {
        self.economy.building(b.kind).is_some_and(|d| d.hub)
    }

    /// Nächstes eigenes Lager zu einem Punkt, das diese Ware lagert.
    pub(super) fn nearest_hub(&self, owner: u32, p: (f32, f32), good: Good) -> Option<u32> {
        self.buildings
            .iter()
            .filter(|b| b.owner == owner && self.is_hub(b) && stores(b, good))
            .min_by(|a, b| dist_to(a, p).total_cmp(&dist_to(b, p)))
            .map(|b| b.id)
    }

    /// Mittelpunkt und Reichweite des Wagenschuppens eines Karrens (0 = überall).
    fn service_area(&self, home: Option<u32>) -> Option<((f32, f32), f32)> {
        let b = self.buildings.iter().find(|b| Some(b.id) == home)?;
        let r = self.economy.building(b.kind).map_or(0, |d| d.service_radius);
        Some((center(b), r as f32))
    }

    /// Bester Auftrag für eine freie Einheit: der nächste, den sie erledigen kann.
    fn find_job(&self, k: usize, reserved: &Reserved, index: &HashMap<u32, usize>) -> Option<Job> {
        let u = &self.units[k];
        let owner = u.owner;
        let cap = self.capacity(u.kind);
        let cart = u.kind == UnitKind::Cart;
        let cart_min = self.economy.def.transport.cart_min_load;
        let min_load = if cart { cart_min } else { 1 };
        let stock = self.stocks.get(&owner)?;
        // Träger und Karren arbeiten nur im Umkreis ihres Schuppens
        // und nur mit den Waren, die dort ausgewählt sind.
        let area = self.service_area(u.home);
        let home = u.home.and_then(|h| index.get(&h)).map(|&i| &self.buildings[i]);
        let moves = |g: Good| home.is_none_or(|h| stores(h, g));
        // Große Ladungen lassen Träger für einen freien Karren liegen, der dort hinkommt.
        let free_carts: Vec<_> = if cart {
            Vec::new()
        } else {
            self.units
                .iter()
                .filter(|c| c.owner == owner && c.kind == UnitKind::Cart && c.job.is_none() && !self.idle_units.contains(&c.id))
                .map(|c| self.service_area(c.home))
                .collect()
        };
        let mut best: Option<(f32, Job)> = None;
        let offer = |best: &mut Option<(f32, Job)>, d: f32, job: Job| {
            if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                *best = Some((d, job));
            }
        };
        // Für Sammelfahrten: gleiche Betriebe mit derselben Ware.
        let mut groups: Groups = HashMap::new();
        for b in self.buildings.iter().filter(|b| b.owner == owner) {
            let Some(inv) = self.inventories.get(&b.id) else { continue };
            if !within(area, b) {
                continue;
            }
            let inputs: &[GoodAmount] =
                b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).map_or(&[], |r| &r.inputs);
            let d = dist_to(b, u.pos);
            // Abholen: alles, was kein Eingang ist, wenn ein Lager es nimmt.
            for g in inv.state().goods {
                if !moves(g.good) || inputs.iter().any(|i| i.good == g.good) || self.nearest_hub(owner, u.pos, g.good).is_none() {
                    continue;
                }
                let free = g.amount.saturating_sub(Reserved::get(&reserved.pickup, (b.id, g.good)));
                let room = stock.capacity.saturating_sub(stock.amount(g.good));
                let amount = free.min(cap).min(room);
                if free >= cart_min && free_carts.iter().any(|a| within(*a, b)) {
                    continue;
                }
                if cart && amount > 0 {
                    groups.entry((b.kind, g.good)).or_default().push((d, b.id, amount));
                }
                if amount >= min_load {
                    let job = Job {
                        kind: JobKind::Pickup { from: b.id },
                        good: g.good,
                        amount,
                        stage: Stage::Load(b.id),
                        rest: NO_REST,
                    };
                    offer(&mut best, d, job);
                }
            }
            // Beliefern: Eingänge für zwei Durchläufe, aber nicht unter den Mindestbestand.
            if !b.settings.accept_deliveries || b.settings.paused {
                continue;
            }
            for i in inputs.iter().filter(|i| moves(i.good)) {
                let Some(hub) = self.nearest_hub(owner, u.pos, i.good) else { continue };
                let want = (i.amount * 2).min(inv.capacity);
                let have = inv.amount(i.good) + Reserved::get(&reserved.incoming, (b.id, i.good));
                let keep = b.settings.keep_in_stock.iter().find(|g| g.good == i.good).map_or(0, |g| g.amount);
                let there = stock
                    .amount(i.good)
                    .saturating_sub(Reserved::get(&reserved.central, (owner, i.good)))
                    .saturating_sub(keep);
                let amount = want.saturating_sub(have).min(cap).min(there);
                if amount >= min_load {
                    // Erst zum Lager, dann zum Betrieb: Weg über das Lager zählt.
                    let via = index.get(&hub).map_or(0.0, |&h| dist_to(&self.buildings[h], u.pos));
                    let job = Job {
                        kind: JobKind::Supply { to: b.id },
                        good: i.good,
                        amount,
                        stage: Stage::Load(hub),
                        rest: NO_REST,
                    };
                    offer(&mut best, via + d, job);
                }
            }
        }
        // Karren: reicht ein Betrieb allein nicht, mehrere gleiche nacheinander anfahren.
        if cart && best.is_none() {
            for ((_, good), mut list) in groups {
                list.sort_by(|a, b| a.0.total_cmp(&b.0));
                let room = stock.capacity.saturating_sub(stock.amount(good)).min(cap);
                let mut stops: Vec<(u32, u32)> = Vec::new();
                let mut total = 0;
                for &(_, id, n) in list.iter().take(TOUR_STOPS) {
                    let n = n.min(room - total);
                    if n == 0 {
                        break;
                    }
                    stops.push((id, n));
                    total += n;
                }
                if total >= cart_min && stops.len() > 1 {
                    let mut rest = NO_REST;
                    for (slot, &stop) in rest.iter_mut().zip(&stops[1..]) {
                        *slot = stop;
                    }
                    let (first, n) = stops[0];
                    let job = Job { kind: JobKind::Pickup { from: first }, good, amount: n, stage: Stage::Load(first), rest };
                    offer(&mut best, list[0].0, job);
                }
            }
        }
        best.map(|(_, j)| j)
    }

    fn start(&mut self, k: usize, job: Job, index: &HashMap<u32, usize>) {
        let Stage::Load(at) = job.stage else { return };
        self.units[k].job = Some(job);
        if !self.route(k, at, index) {
            self.fail(k, index);
        }
    }

    /// Weg ist zu Ende: aufladen, abladen oder aufgeben.
    fn arrive(&mut self, k: usize, job: Job, index: &HashMap<u32, usize>) {
        let (Stage::Load(at) | Stage::Unload(at)) = job.stage;
        let Some(&bi) = index.get(&at) else {
            return self.fail(k, index);
        };
        if dist_to(&self.buildings[bi], self.units[k].pos) > REACH {
            return self.fail(k, index);
        }
        let owner = self.units[k].owner;
        match job.stage {
            Stage::Load(_) => {
                let taken = match job.kind {
                    JobKind::Pickup { from } => {
                        let Some(inv) = self.inventories.get_mut(&from) else {
                            return self.fail(k, index);
                        };
                        let n = job.amount.min(inv.amount(job.good));
                        inv.take_all(&[GoodAmount {
                            good: job.good,
                            amount: n,
                        }]);
                        self.dirty_buildings.insert(from);
                        n
                    }
                    JobKind::Supply { .. } => {
                        let Some(stock) = self.stocks.get_mut(&owner) else {
                            return self.fail(k, index);
                        };
                        let n = job.amount.min(stock.amount(job.good));
                        stock.take_all(&[GoodAmount {
                            good: job.good,
                            amount: n,
                        }]);
                        self.dirty_stocks.insert(owner);
                        n
                    }
                };
                let carried = self.units[k].carrying.map_or(0, |c| c.amount) + taken;
                if carried == 0 {
                    self.units[k].job = None;
                    return;
                }
                self.units[k].carrying = Some(GoodAmount { good: job.good, amount: carried });
                // Sammelfahrt: weiter zum nächsten Betrieb.
                if let (JobKind::Pickup { .. }, (next, n)) = (job.kind, job.rest[0]) {
                    if next != 0 {
                        let mut rest = NO_REST;
                        rest[..TOUR_STOPS - 2].copy_from_slice(&job.rest[1..]);
                        self.units[k].job = Some(Job { kind: JobKind::Pickup { from: next }, amount: n, stage: Stage::Load(next), rest, ..job });
                        if self.route(k, next, index) {
                            return;
                        }
                    }
                }
                let dest = match job.kind {
                    JobKind::Pickup { .. } => self.nearest_hub(owner, self.units[k].pos, job.good),
                    JobKind::Supply { to } => Some(to),
                };
                let Some(dest) = dest else {
                    return self.fail(k, index);
                };
                self.units[k].job = Some(Job { amount: carried, stage: Stage::Unload(dest), rest: NO_REST, ..job });
                if !self.route(k, dest, index) {
                    self.fail(k, index);
                }
            }
            Stage::Unload(_) => {
                let Some(load) = self.units[k].carrying else {
                    self.units[k].job = None;
                    return;
                };
                let left = match job.kind {
                    JobKind::Pickup { .. } => {
                        let Some(stock) = self.stocks.get_mut(&owner) else {
                            return;
                        };
                        let fits = stock.add(load.good, load.amount);
                        self.dirty_stocks.insert(owner);
                        load.amount - fits
                    }
                    JobKind::Supply { to } => {
                        let Some(inv) = self.inventories.get_mut(&to) else {
                            return self.fail(k, index);
                        };
                        let fits = inv.add(load.good, load.amount);
                        self.dirty_buildings.insert(to);
                        load.amount - fits
                    }
                };
                if left == 0 {
                    self.units[k].carrying = None;
                    self.units[k].job = None;
                    return;
                }
                self.units[k].carrying = Some(GoodAmount {
                    good: load.good,
                    amount: left,
                });
                if let JobKind::Supply { .. } = job.kind {
                    // Passt nicht mehr hinein: Rest zurück ins Lager.
                    self.return_load(k, index);
                } else {
                    // Vorrat voll: warten und es gleich noch einmal versuchen.
                    self.units[k].cooldown = IDLE_WAIT;
                }
            }
        }
    }

    /// Ladung zurück zum nächsten Lager bringen.
    fn return_load(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let Some(load) = self.units[k].carrying else {
            return;
        };
        let u = &self.units[k];
        let hub = self.nearest_hub(u.owner, u.pos, load.good);
        let job = hub.map(|h| Job {
            kind: JobKind::Pickup { from: 0 },
            good: load.good,
            amount: load.amount,
            stage: Stage::Unload(h),
            rest: NO_REST,
        });
        self.units[k].job = job;
        if !hub.is_some_and(|h| self.route(k, h, index)) {
            // Kein Lager erreichbar: Ware zählt trotzdem zum Vorrat, damit nichts verloren geht.
            let owner = self.units[k].owner;
            if let Some(stock) = self.stocks.get_mut(&owner) {
                stock.add(load.good, load.amount);
                self.dirty_stocks.insert(owner);
            }
            self.units[k].carrying = None;
            self.units[k].job = None;
        }
    }

    /// Auftrag geht nicht (Ziel weg oder nicht erreichbar).
    fn fail(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let back_to_hub = matches!(
            self.units[k].job,
            Some(Job {
                stage: Stage::Load(_),
                ..
            })
        ) || matches!(
            self.units[k].job,
            Some(Job {
                kind: JobKind::Supply { .. },
                ..
            })
        );
        self.units[k].job = None;
        self.units[k].cooldown = FAIL_WAIT;
        if self.units[k].carrying.is_some() {
            if back_to_hub {
                self.return_load(k, index);
            } else {
                // Schon auf dem Weg zum Lager gescheitert: nicht endlos im Kreis laufen.
                let u = &self.units[k];
                let (owner, load) = (u.owner, u.carrying.expect("eben geprüft"));
                if let Some(stock) = self.stocks.get_mut(&owner) {
                    stock.add(load.good, load.amount);
                    self.dirty_stocks.insert(owner);
                }
                self.units[k].carrying = None;
            }
        }
    }

    fn go_home(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let Some(home) = self.units[k].home else {
            return;
        };
        let Some(&bi) = index.get(&home) else { return };
        if dist_to(&self.buildings[bi], self.units[k].pos) > REACH {
            self.route(k, home, index);
        }
    }

    /// Weg zum Gebäude planen, bis an ein freies Feld daneben.
    fn route(&mut self, k: usize, building: u32, index: &HashMap<u32, usize>) -> bool {
        let Some(&bi) = index.get(&building) else {
            return false;
        };
        let pos = self.units[k].pos;
        let Some(door) = self.door(&self.buildings[bi], pos) else {
            return false;
        };
        let u = &mut self.units[k];
        u.path = self.map.nav.waypoints(pos, door);
        u.target = Some(door);
        true
    }

    /// Freies Feld am Rand eines Gebäudes, das `from` am nächsten liegt.
    pub(super) fn door(&self, b: &BuildingState, from: (f32, f32)) -> Option<(f32, f32)> {
        // Gebäude mit Eingang: immer dorthin.
        if let Some((ex, ey)) = b.entrance {
            if self.map.nav.is_open(ex as i32, ey as i32) {
                return Some((ex as f32 + 0.5, ey as f32 + 0.5));
            }
        }
        let (x0, y0, x1, y1) = (
            b.x as i32 - 1,
            b.y as i32 - 1,
            (b.x + b.w) as i32,
            (b.y + b.h) as i32,
        );
        let ring = (x0..=x1)
            .flat_map(|x| [(x, y0), (x, y1)])
            .chain((y0 + 1..y1).flat_map(|y| [(x0, y), (x1, y)]));
        ring.filter(|&(x, y)| self.map.nav.is_open(x, y))
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .min_by(|a, b| {
                let d = |p: &(f32, f32)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
                d(a).total_cmp(&d(b))
            })
    }

    /// Neuer Träger oder Karren an einem Gebäude.
    pub(super) fn spawn_transport(&mut self, b: &BuildingState, kind: UnitKind, n: u32) {
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let Some(pos) = self.door(b, center) else {
            return;
        };
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        let mut u = super::Unit::new(id, b.owner, kind, pos);
        u.home = Some(b.id);
        // Nicht alle auf einmal losschicken.
        u.cooldown = 0.3 * n as f32;
        self.units.push(u);
    }

    /// Träger, die auf der Fläche eines neuen Gebäudes stehen, treten zur Seite.
    pub(super) fn make_room(&mut self, b: &BuildingState) {
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let door = self.door(b, center);
        for u in self.units.iter_mut().filter(|u| u.is_civilian()) {
            let inside = u.pos.0 >= b.x as f32
                && u.pos.1 >= b.y as f32
                && u.pos.0 < (b.x + b.w) as f32
                && u.pos.1 < (b.y + b.h) as f32;
            if let (true, Some(door)) = (inside, door) {
                u.pos = door;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, BuildingStatus, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn flat(w: usize, h: usize) -> World {
        let row = "0".repeat(w);
        let text = format!("spawn 0 {}\n{}", h - 1, format!("{row}\n").repeat(h));
        let mut world = World::new(
            GameMap::parse(&text).unwrap(),
            Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people(),
        );
        world.spawn_player(1);
        world
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as usize {
            w.step(0.05);
        }
    }

    fn central(w: &World, g: Good) -> u32 {
        w.stocks[&1].amount(g)
    }

    fn local(w: &World, id: u32, g: Good) -> u32 {
        w.inventories[&id].amount(g)
    }

    /// Wagenschuppen auf Stufe `level` (1 = Träger, 2 = dazu ein Karren).
    fn shed(w: &mut World, x: u32, y: u32, level: u8) -> u32 {
        w.place_building_rotated(1, BuildingKind::CartShed, x, y, 0, level).unwrap().id
    }

    fn count(w: &World, kind: UnitKind) -> usize {
        w.units.iter().filter(|u| u.kind == kind).count()
    }

    #[test]
    fn traeger_und_karren_nur_aus_dem_wagenschuppen() {
        let mut w = flat(30, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        w.place_building(1, BuildingKind::Storehouse, 12, 2).unwrap();
        assert_eq!(count(&w, UnitKind::Carrier), 0, "Lager bringen keine Träger mehr mit");
        let shed = shed(&mut w, 20, 2, 1);
        assert_eq!((count(&w, UnitKind::Carrier), count(&w, UnitKind::Cart)), (3, 0), "Stufe 1: Träger");
        w.upgrade(1, shed).unwrap();
        assert_eq!((count(&w, UnitKind::Carrier), count(&w, UnitKind::Cart)), (3, 1), "Stufe 2: dazu der Karren");
        let more: Vec<GoodAmount> = [Good::Wood, Good::Boards, Good::Blocks].iter().map(|&good| GoodAmount { good, amount: 200 }).collect();
        w.admin_give(1, &more, 0);
        w.upgrade(1, shed).unwrap();
        w.upgrade(1, shed).unwrap();
        assert_eq!(count(&w, UnitKind::Cart), 3, "Stufe 3 und 4: je ein Karren mehr");
        // Träger stehen nie in Gebäuden, und befehlen lassen sie sich nicht.
        let id = w.units.iter().find(|u| u.kind == UnitKind::Carrier).unwrap().id;
        w.order_move(1, &[id], (29.5, 19.5));
        assert!(w.units.iter().find(|u| u.id == id).unwrap().path.is_empty());
        w.demolish(1, shed).unwrap();
        assert_eq!(count(&w, UnitKind::Carrier) + count(&w, UnitKind::Cart), 0);
    }

    #[test]
    fn wagenschuppen_bewegt_nur_ausgewaehlte_waren() {
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let s = shed(&mut w, 10, 12, 1);
        let mut set = w.buildings.iter().find(|b| b.id == s).unwrap().settings.clone();
        set.stores = Some(vec![Good::Wood]);
        w.configure(1, s, set).unwrap();
        let orchard = w.place_building(1, BuildingKind::Orchard, 20, 2).unwrap();
        pause(&mut w, orchard.id);
        set_fruit(&mut w, orchard.id, 4);
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 4, "Obst ist nicht ausgewählt");
        let mut set = w.buildings.iter().find(|b| b.id == s).unwrap().settings.clone();
        set.stores = None;
        w.configure(1, s, set).unwrap();
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 0, "jetzt alles");
    }

    #[test]
    fn traeger_holen_ab_und_beliefern() {
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let cutter = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        shed(&mut w, 10, 12, 1);
        let wood = central(&w, Good::Wood);
        let boards = central(&w, Good::Boards);
        // Sägewerk: 2 Holz → 2 Bretter in 8 s (siehe economy.json). Ohne Lieferung fehlt Holz.
        assert_eq!(local(&w, cutter.id, Good::Wood), 0);
        run(&mut w, 60.0);
        assert!(central(&w, Good::Wood) < wood, "Holz ging zum Sägewerk");
        assert!(
            central(&w, Good::Boards) > boards,
            "Bretter kamen zurück in den Vorrat"
        );
        let b = w.buildings.iter().find(|b| b.id == cutter.id).unwrap();
        assert_ne!(b.status, BuildingStatus::StorageFull, "Bretter werden abgeholt");
    }

    #[test]
    fn ohne_lieferungen_nur_eigener_vorrat() {
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        let mut s = saw.settings.clone();
        s.accept_deliveries = false;
        w.configure(1, saw.id, s).unwrap();
        let wood = central(&w, Good::Wood);
        run(&mut w, 30.0);
        assert_eq!(central(&w, Good::Wood), wood, "nichts geliefert");
        assert_eq!(
            w.buildings.iter().find(|b| b.id == saw.id).unwrap().status,
            BuildingStatus::MissingInput
        );
    }

    #[test]
    fn ohne_lager_staut_es_sich_und_karren_faehrt_erst_ab_mindestmenge() {
        let mut w = flat(40, 20);
        let orchard = w.place_building(1, BuildingKind::Orchard, 30, 2).unwrap();
        // Kein Lager: Obst bleibt liegen, bis das eigene Lager voll ist.
        run(&mut w, 25.0 * 7.0 + 5.0);
        let cap = w.economy.def.transport.building_capacity;
        assert_eq!(local(&w, orchard.id, Good::Fruit), cap);
        assert_eq!(
            w.buildings
                .iter()
                .find(|b| b.id == orchard.id)
                .unwrap()
                .status,
            BuildingStatus::StorageFull
        );
        // Bergfried mit Trägern und ein Wagenschuppen: Die große Ladung lassen
        // die Träger für den Karren liegen, er holt alles auf einmal.
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        shed(&mut w, 10, 2, 2);
        let fruit = central(&w, Good::Fruit);
        let mut cart_load = 0;
        for _ in 0..20 * 40 {
            w.step(0.05);
            let cart = w.units.iter().find(|u| u.kind == UnitKind::Cart).unwrap();
            cart_load = cart_load.max(cart.carrying.map_or(0, |g| g.amount));
        }
        assert_eq!(cart_load, cap, "Karren holt alles auf einmal");
        assert!(central(&w, Good::Fruit) >= fruit + cap);
        // Ohne Träger: Kleinkram lässt auch der Karren liegen.
        w.units.retain(|u| u.kind != UnitKind::Carrier);
        // Wenig liegt herum: dafür fährt der Karren nicht.
        let min = w.economy.def.transport.cart_min_load;
        let left = local(&w, orchard.id, Good::Fruit);
        let inv = w.inventories.get_mut(&orchard.id).unwrap();
        inv.take_all(&[GoodAmount {
            good: Good::Fruit,
            amount: left,
        }]);
        inv.add(Good::Fruit, min - 1);
        let mut s = orchard.settings.clone();
        s.paused = true;
        w.configure(1, orchard.id, s).unwrap();
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), min - 1);
    }

    fn set_fruit(w: &mut World, id: u32, n: u32) {
        let inv = w.inventories.get_mut(&id).unwrap();
        let have = inv.amount(Good::Fruit);
        inv.take_all(&[GoodAmount { good: Good::Fruit, amount: have }]);
        inv.add(Good::Fruit, n);
    }

    fn pause(w: &mut World, id: u32) {
        let mut s = w.buildings.iter().find(|b| b.id == id).unwrap().settings.clone();
        s.paused = true;
        w.configure(1, id, s).unwrap();
    }

    #[test]
    fn lager_nimmt_nur_gewaehlte_waren() {
        let mut w = flat(40, 20);
        let keep = w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let mut s = keep.settings.clone();
        s.stores = Some(vec![Good::Wood]);
        w.configure(1, keep.id, s).unwrap();
        let orchard = w.place_building(1, BuildingKind::Orchard, 20, 2).unwrap();
        shed(&mut w, 10, 12, 1);
        pause(&mut w, orchard.id);
        set_fruit(&mut w, orchard.id, 4);
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 4, "kein Lager nimmt Obst");
        w.place_building(1, BuildingKind::Storehouse, 30, 12).unwrap();
        run(&mut w, 40.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 0, "jetzt ins Vorratslager");
    }

    #[test]
    fn mindestbestand_bleibt_im_vorrat() {
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        let wood = central(&w, Good::Wood);
        let mut s = saw.settings.clone();
        s.keep_in_stock = vec![GoodAmount { good: Good::Wood, amount: wood - 2 }];
        w.configure(1, saw.id, s).unwrap();
        // Ohne Wagenschuppen holt der Arbeiter des Sägewerks selbst.
        run(&mut w, 40.0);
        assert_eq!(central(&w, Good::Wood), wood - 2, "nur die 2 über dem Mindestbestand");
    }

    #[test]
    fn karren_sammelt_bei_mehreren_und_bleibt_im_umkreis() {
        let mut w = flat(120, 30);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        shed(&mut w, 12, 2, 2);
        w.units.retain(|u| u.kind != UnitKind::Carrier);
        let a = w.place_building(1, BuildingKind::Orchard, 24, 2).unwrap();
        let b = w.place_building(1, BuildingKind::Orchard, 32, 2).unwrap();
        // Weit außerhalb des Umkreises (40 Felder): wird nie angefahren.
        let far = w.place_building(1, BuildingKind::Orchard, 100, 2).unwrap();
        for id in [a.id, b.id, far.id] {
            pause(&mut w, id);
        }
        set_fruit(&mut w, a.id, 5);
        set_fruit(&mut w, b.id, 5);
        set_fruit(&mut w, far.id, 12);
        let mut most = 0;
        for _ in 0..20 * 90 {
            w.step(0.05);
            let cart = w.units.iter().find(|u| u.kind == UnitKind::Cart).unwrap();
            most = most.max(cart.carrying.map_or(0, |g| g.amount));
        }
        assert_eq!(most, 10, "beide Plantagen auf einer Fahrt");
        assert_eq!(local(&w, far.id, Good::Fruit), 12);
    }

    #[test]
    fn abriss_gibt_inhalt_in_den_vorrat() {
        let mut w = flat(30, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 14, 2).unwrap();
        w.inventories.get_mut(&saw.id).unwrap().add(Good::Boards, 5);
        let boards = central(&w, Good::Boards);
        w.demolish(1, saw.id).unwrap();
        // 5 aus dem Lager plus Hälfte der Baukosten (keine Bretter beim Sägewerk).
        assert_eq!(central(&w, Good::Boards), boards + 5);
    }
}
