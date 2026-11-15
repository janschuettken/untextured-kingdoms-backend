//! Computergegner.
//!
//! Ein Computergegner sitzt auf einem Startplatz wie ein Mensch und spielt mit
//! denselben Befehlen (`ClientMsg`), die auch der Browser schickt. Die gehen durch
//! `game::apply_command`, also gelten genau dieselben Regeln und Kosten: er schummelt
//! nicht. Lesen darf er nur, was ein Spieler auch sieht (Karte, Gebäude, eigenes Lager).
//!
//! Aufbau:
//! - `profile.rs`: wie stark er ist und was er baut (Daten, leicht erweiterbar).
//! - `layout.rs`: wo er baut (Grundriss mit Mauerring, Straßen und Abständen).
//! - hier: der Ablauf, der alle paar Sekunden überlegt, was als Nächstes dran ist.
//!
//! Er überlegt jedes Mal neu aus dem Spielstand. Darum muss außer der Stärke nichts
//! gespeichert werden; nach dem Laden eines Spiels macht er einfach weiter.

mod layout;
mod profile;

use std::collections::HashMap;

use protocol::{AiLevel, BuildingCategory, BuildingKind, ClientMsg, Good, GoodAmount, StockState, Text};
use tracing::debug;

use crate::world::{level_cost, World};
use layout::Layout;
pub use profile::Profile;

/// So lange (Spielsekunden) lässt er ein Gebäude aus, für das er keinen Platz gefunden hat.
const RETRY_SECONDS: f32 = 90.0;
/// Liegt ein Abbaubetrieb weiter als so viele Felder vom nächsten Lager, baut er dort eins.
const HUB_DISTANCE: f32 = 22.0;
/// Soldaten, die weiter als so viele Felder vom Sammelpunkt stehen, holt er zurück.
const RALLY_SLACK: f32 = 7.0;

/// Ein Computergegner in einem laufenden Spiel.
#[derive(Debug, Clone)]
pub struct Computer {
    pub player: u32,
    profile: Profile,
    /// Spielzeit seit Spielbeginn (bzw. seit dem Laden).
    clock: f32,
    /// Spielzeit der nächsten Überlegung.
    next_think: f32,
    /// Gebäude ohne passenden Platz: erst ab dieser Zeit wieder versuchen.
    skip_until: HashMap<BuildingKind, f32>,
    /// Tore, die nicht gebaut werden konnten (Index in `Layout::gates`), bis zu dieser Zeit.
    gate_skip: HashMap<usize, f32>,
    /// Welcher Soldat aus `army_units` als Nächstes dran ist.
    next_unit: usize,
}

impl Computer {
    pub fn new(player: u32, level: AiLevel) -> Computer {
        let profile = Profile::for_level(level);
        Computer {
            player,
            // Leicht versetzt, damit nicht alle Computergegner im selben Tick rechnen.
            next_think: 1.0 + (player % 4) as f32 * 0.5,
            profile,
            clock: 0.0,
            skip_until: HashMap::new(),
            gate_skip: HashMap::new(),
            next_unit: 0,
        }
    }

    /// Einen Spieltakt von `dt` Spielsekunden weiter. Liefert die Befehle, die er geben will.
    pub fn think(&mut self, world: &World, dt: f32) -> Vec<ClientMsg> {
        self.clock += dt;
        if self.clock < self.next_think {
            return Vec::new();
        }
        self.next_think = self.clock + self.profile.think_seconds;
        let Some(keep) = world.buildings.iter().find(|b| b.owner == self.player && b.kind == BuildingKind::Keep) else {
            return Vec::new();
        };
        let layout = Layout::new(world, keep, self.profile.wall_gap);
        let stock = world.stock_state(self.player);
        let mut cmds = Vec::new();
        // Erst die Bauliste, dann der Mauerring, dann Ausbauten.
        if !self.build_next(world, &layout, &stock, &mut cmds) && !self.build_walls(world, &layout, &stock, &mut cmds) {
            self.upgrade(world, &stock, &mut cmds);
        }
        self.configure(world, &mut cmds);
        self.train(world, &stock, &mut cmds);
        self.rally(world, &layout, &mut cmds);
        cmds
    }

    /// Ein Befehl wurde abgelehnt (sollte selten sein, er prüft vorher).
    pub fn rejected(&mut self, text: &Text) {
        debug!("Computer {}: abgelehnt: {}", self.player, text.key);
    }

    fn count(&self, world: &World, kind: BuildingKind) -> u32 {
        world.buildings.iter().filter(|b| b.owner == self.player && b.kind == kind).count() as u32
    }

    /// Reicht das Lager für die erste Stufe?
    fn affordable(&self, world: &World, stock: &StockState, kind: BuildingKind) -> bool {
        let Some(def) = world.economy.building(kind) else { return false };
        if def.first_free && self.count(world, kind) == 0 {
            return true;
        }
        level_cost(def, 1).iter().all(|c| have(stock, c.good) >= c.amount)
    }

    /// Das nächste fällige Gebäude bauen. `true`, wenn er auf etwas wartet oder gebaut hat
    /// (dann keine Mauern, damit das Material für die Wirtschaft bleibt).
    fn build_next(&mut self, world: &World, layout: &Layout, stock: &StockState, cmds: &mut Vec<ClientMsg>) -> bool {
        let Some((kind, near)) = self.next_goal(world, stock) else { return false };
        if !self.affordable(world, stock, kind) {
            return true;
        }
        match layout::find_site(world, self.player, kind, layout, near) {
            Some((x, y, rotation)) => {
                debug!("Computer {}: baut {kind:?} bei {x},{y}", self.player);
                cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation, level: 1 });
            }
            None => {
                debug!("Computer {}: kein Platz für {kind:?}", self.player);
                self.skip_until.insert(kind, self.clock + RETRY_SECONDS);
            }
        }
        true
    }

    /// Was als Nächstes gebaut werden soll: erst, was fehlt (Wohnung, Essen, Lagerplatz),
    /// dann die Bauliste des Profils.
    /// Liefert das Gebäude und wo es hin soll (`None` = üblicher Ort).
    fn next_goal(&self, world: &World, stock: &StockState) -> Option<(BuildingKind, Option<(i64, i64)>)> {
        let ready = |k: BuildingKind| self.skip_until.get(&k).is_none_or(|&t| self.clock >= t);
        let people = &stock.people;
        // Wohnungen: immer ein paar Plätze frei für die nächsten Betriebe.
        if people.needed + 1 >= people.housing && ready(BuildingKind::House) && self.count(world, BuildingKind::House) > 0 {
            return Some((BuildingKind::House, None));
        }
        // Hunger: noch eine Obstplantage.
        let orchards = self.count(world, BuildingKind::Orchard);
        if people.fed < people.working && orchards > 0 && orchards < people.working / 6 + 1 && ready(BuildingKind::Orchard) {
            return Some((BuildingKind::Orchard, None));
        }
        // Abbaubetrieb weit weg vom nächsten Lager: dort ein Vorratslager, sonst laufen sich
        // die Arbeiter die Füße wund.
        for (category, hub) in [(BuildingCategory::Raw, BuildingKind::Storehouse), (BuildingCategory::Food, BuildingKind::Granary)] {
            if self.count(world, hub) > 0 && ready(hub) {
                if let Some(p) = self.far_from_hub(world, category, hub) {
                    return Some((hub, Some(p)));
                }
            }
        }
        // Lager fast voll: noch ein Vorratslager.
        let full = stock.goods.iter().any(|g| stock.capacity > 0 && g.amount * 10 >= stock.capacity * 9);
        if full && self.count(world, BuildingKind::Storehouse) < 4 && ready(BuildingKind::Storehouse) {
            return Some((BuildingKind::Storehouse, None));
        }
        self.profile
            .build_order
            .iter()
            .find(|&&(k, n)| self.count(world, k) < n && ready(k))
            .map(|&(k, _)| (k, None))
    }

    /// Mitte eines eigenen Betriebs dieser Kategorie, der weit vom nächsten passenden Lager
    /// liegt (Rohstoffe: Bergfried oder Vorratslager; Essen: Kornspeicher).
    fn far_from_hub(&self, world: &World, category: BuildingCategory, hub: BuildingKind) -> Option<(i64, i64)> {
        let center = |b: &protocol::BuildingState| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let own = || world.buildings.iter().filter(|b| b.owner == self.player);
        let fits = |k: BuildingKind| k == hub || (hub == BuildingKind::Storehouse && k == BuildingKind::Keep);
        let hubs: Vec<(f32, f32)> = own().filter(|b| fits(b.kind)).map(center).collect();
        own()
            .filter(|b| world.economy.building(b.kind).is_some_and(|d| d.category == category))
            .map(center)
            .find(|p| hubs.iter().all(|h| (h.0 - p.0).hypot(h.1 - p.1) > HUB_DISTANCE))
            .map(|p| (p.0 as i64, p.1 as i64))
    }

    /// Höchstbestände aus dem Profil in neue Betriebe eintragen.
    fn configure(&self, world: &World, cmds: &mut Vec<ClientMsg>) {
        for b in world.buildings.iter().filter(|b| b.owner == self.player && b.settings.stop_at.is_empty()) {
            let Some(recipe) = b.settings.recipe.as_deref().and_then(|r| world.economy.recipe(r)) else { continue };
            let stop_at: Vec<GoodAmount> = recipe
                .outputs
                .iter()
                .filter_map(|o| self.profile.stock_targets.iter().find(|t| t.0 == o.good))
                .map(|&(good, amount)| GoodAmount { good, amount })
                .collect();
            if !stop_at.is_empty() {
                let mut settings = b.settings.clone();
                settings.stop_at = stop_at;
                cmds.push(ClientMsg::ConfigureBuilding { building_id: b.id, settings });
            }
        }
    }

    /// Ausbauten aus dem Profil, eins pro Überlegung.
    fn upgrade(&self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        for &(kind, level) in &self.profile.upgrades {
            let Some(def) = world.economy.building(kind) else { continue };
            let Some(b) = world.buildings.iter().find(|b| b.owner == self.player && b.kind == kind && b.level < level) else {
                continue;
            };
            let Some(next) = def.levels.get(b.level as usize) else { continue };
            if next.cost.iter().all(|c| have(stock, c.good) >= c.amount) {
                cmds.push(ClientMsg::UpgradeBuilding { building_id: b.id });
            }
            return;
        }
    }

    /// Mauerring mit Toren, ein paar Stücke pro Überlegung. Erst wenn die Kaserne steht.
    /// `true`, solange am Ring noch etwas zu tun ist (dann wartet er mit Ausbauten).
    fn build_walls(&mut self, world: &World, layout: &Layout, stock: &StockState, cmds: &mut Vec<ClientMsg>) -> bool {
        if self.count(world, BuildingKind::Barracks) == 0 {
            return false;
        }
        let p = self.player;
        // Lokaler Vorrat: was die Befehle dieser Runde schon verbrauchen.
        let mut left: HashMap<Good, u32> = stock.goods.iter().map(|g| (g.good, g.amount)).collect();
        let reserve = self.profile.wall_wood_reserve;
        let can_pay = |world: &World, kind: BuildingKind, left: &HashMap<Good, u32>| -> bool {
            let Some(def) = world.economy.building(kind) else { return false };
            level_cost(def, 1).iter().all(|c| {
                let keep = if c.good == Good::Wood { reserve } else { 0 };
                left.get(&c.good).copied().unwrap_or(0) >= c.amount + keep
            })
        };
        let pay = |world: &World, kind: BuildingKind, left: &mut HashMap<Good, u32>| {
            for c in world.economy.building(kind).map(|d| level_cost(d, 1)).unwrap_or_default() {
                *left.entry(c.good).or_default() -= c.amount;
            }
        };
        // Tore zuerst, sonst sperrt er sich selbst ein.
        let gates = layout.gates();
        let own_gate_at = |x: u32, y: u32| {
            world.buildings.iter().any(|b| b.owner == p && b.kind == BuildingKind::Gate && b.x == x && b.y == y)
        };
        for (i, &(x, y, rotation)) in gates.iter().enumerate() {
            if own_gate_at(x, y) || self.gate_skip.get(&i).is_some_and(|&t| self.clock < t) {
                continue;
            }
            if !can_pay(world, BuildingKind::Gate, &left) {
                return true;
            }
            if world.check_placement(p, BuildingKind::Gate, (x, y), rotation, 1).is_err() {
                self.gate_skip.insert(i, self.clock + RETRY_SECONDS);
                continue;
            }
            cmds.push(ClientMsg::PlaceBuilding { kind: BuildingKind::Gate, x, y, rotation, level: 1 });
            return true;
        }
        // Ohne ein einziges Tor keine Mauer.
        if !gates.iter().any(|&(x, y, _)| own_gate_at(x, y)) {
            return false;
        }
        let kind = self.profile.wall_kind;
        let mut placed = 0;
        for (x, y) in layout.ring_tiles() {
            if x < 0 || y < 0 || layout.is_gate_tile(x, y) {
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            // Schon eine eigene Mauer (oder ein anderes Gebäude) dort.
            if world.buildings.iter().any(|b| x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h) {
                continue;
            }
            if placed >= self.profile.walls_per_think || !can_pay(world, kind, &left) {
                return true;
            }
            if world.check_placement(p, kind, (x, y), 0, 1).is_err() {
                continue;
            }
            pay(world, kind, &mut left);
            cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation: 0, level: 1 });
            placed += 1;
        }
        placed > 0
    }

    /// Soldaten in Auftrag geben, bis die Armee groß genug ist.
    fn train(&mut self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        let units = &self.profile.army_units;
        if units.is_empty() {
            return;
        }
        let barracks: Vec<_> = world.buildings.iter().filter(|b| b.owner == self.player && b.kind == BuildingKind::Barracks).collect();
        let queued: u32 = barracks.iter().map(|b| b.training.len() as u32).sum();
        if stock.army.soldiers + queued >= self.profile.army_size {
            return;
        }
        let queue_max = world.economy.def.army.queue_max as usize;
        // Der Reihe nach; kann er den dran ist nicht ausbilden, den nächsten versuchen.
        for step in 0..units.len() {
            let kind = units[(self.next_unit + step) % units.len()];
            let Some(def) = world.economy.unit(kind) else { continue };
            if stock.taler < def.taler || def.cost.iter().any(|c| have(stock, c.good) < c.amount) {
                continue;
            }
            let trains = |b: &&&protocol::BuildingState| {
                world.economy.building(b.kind).is_some_and(|d| {
                    d.levels.iter().take(b.level as usize).any(|l| l.trains.contains(&kind))
                })
            };
            let Some(b) = barracks.iter().filter(trains).filter(|b| b.training.len() < queue_max).min_by_key(|b| b.training.len()) else {
                continue;
            };
            cmds.push(ClientMsg::TrainUnits { building_id: b.id, kind, count: 1, spread: false });
            self.next_unit = (self.next_unit + step + 1) % units.len();
            return;
        }
    }

    /// Stehende Soldaten, die weit weg sind, zum Sammelpunkt vor dem Bergfried schicken.
    fn rally(&self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        let Some(point) = rally_point(world, self.player, layout) else { return };
        let ids: Vec<u32> = world
            .units
            .iter()
            .filter(|u| u.owner == self.player && u.kind.is_soldier() && !u.is_moving())
            .filter(|u| (u.pos.0 - point.0).hypot(u.pos.1 - point.1) > RALLY_SLACK)
            .map(|u| u.id)
            .collect();
        if !ids.is_empty() {
            cmds.push(ClientMsg::MoveUnits { unit_ids: ids, x: point.0, y: point.1 });
        }
    }
}

/// Sammelpunkt: ein paar Felder vor dem Eingang des Bergfrieds, auf der Straße.
fn rally_point(world: &World, player: u32, layout: &Layout) -> Option<(f32, f32)> {
    let keep = world.buildings.iter().find(|b| b.owner == player && b.kind == BuildingKind::Keep)?;
    let (ex, ey) = keep.entrance?;
    let (mx, my) = (layout.mid.0 as f32 + 0.5, layout.mid.1 as f32 + 0.5);
    let (dx, dy) = (ex as f32 + 0.5 - mx, ey as f32 + 0.5 - my);
    let len = dx.hypot(dy).max(1.0);
    Some((ex as f32 + 0.5 + dx / len * 3.0, ey as f32 + 0.5 + dy / len * 3.0))
}

fn have(stock: &StockState, good: Good) -> u32 {
    stock.goods.iter().find(|g| g.good == good).map_or(0, |g| g.amount)
}

/// Die Computergegner eines Spiels aus der Lobby (Spieler-Nummer, Stärke).
pub fn computers(list: &[(u32, AiLevel)]) -> Vec<Computer> {
    list.iter().map(|&(p, l)| Computer::new(p, l)).collect()
}

#[cfg(test)]
mod tests;
