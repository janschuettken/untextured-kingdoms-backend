//! Computergegner.
//!
//! Ein Computergegner sitzt auf einem Startplatz wie ein Mensch und spielt mit
//! denselben Befehlen (`ClientMsg`), die auch der Browser schickt. Die gehen durch
//! `game::apply_command`, also gelten genau dieselben Regeln und Kosten: er schummelt
//! nicht. Lesen darf er nur, was ein Spieler auch sieht (Karte, Gebäude, eigenes Lager).
//!
//! Aufbau:
//! - `profile.rs`: wie stark er ist und was er baut (Daten, leicht erweiterbar).
//! - `personality.rs`: Charaktere aus `data/ai_personalities.json` (getrennt von der Stärke).
//! - `attack.rs`: Stärke der Gegner schätzen, Wellen schicken.
//! - `layout.rs`: wo er baut (Grundriss mit Mauerring, Straßen und Abständen).
//! - hier: der Ablauf, der alle paar Sekunden überlegt, was als Nächstes dran ist.
//!
//! Er überlegt jedes Mal neu aus dem Spielstand. Darum muss außer der Stärke nichts
//! gespeichert werden; nach dem Laden eines Spiels macht er einfach weiter.

mod attack;
mod layout;
pub mod personality;
mod profile;

use std::collections::HashMap;

use protocol::{AiLevel, BuildingCategory, BuildingKind, ClientMsg, Good, GoodAmount, StockState, Text, UnitKind};
use tracing::debug;

use crate::world::{level_cost, World};
use layout::{GateSpot, Layout};
use attack::{Campaign, Relief};
use protocol::AiPersonalities;
pub use profile::Profile;

use crate::economy::Economy;

/// Ein Computer auf einem Startplatz, wie er in der Lobby eingestellt ist.
#[derive(Debug, Clone, PartialEq)]
pub struct Seat {
    pub player: u32,
    pub level: AiLevel,
    /// Id des Charakters (leer = der erste).
    pub personality: String,
    /// Eigene Regler für dieses Spiel (0 bis 100), sonst die des Charakters.
    pub daring: Option<u8>,
    pub helpfulness: Option<u8>,
}

/// So lange (Spielsekunden) lässt er ein Gebäude aus, für das er keinen Platz gefunden hat.
const RETRY_SECONDS: f32 = 90.0;
/// Liegt ein Abbaubetrieb weiter als so viele Felder vom nächsten Lager, baut er dort eins.
const HUB_DISTANCE: f32 = 22.0;
/// Ab so viel Holz bzw. Stein in einem Lager draußen baut er die Werkstatt daneben.
const REMOTE_INPUT: u32 = 60;
/// So viel Stein bleibt liegen, bevor er Pflasterwege legt.
const ROAD_STONE_RESERVE: u32 = 25;
/// Feindliche Soldaten näher als so viele Felder am Bergfried: Verteidigung.
const THREAT_RADIUS: f32 = 30.0;
/// So oft prüft er, ob die fertige Mauer dicht ist.
const SEAL_SECONDS: f32 = 30.0;
/// Soldaten, die weiter als so viele Felder vom Sammelpunkt stehen, holt er zurück.
const RALLY_SLACK: f32 = 7.0;
/// Hilfe für Verbündete: so viel stärker als die Angreifer schickt er höchstens.
const RELIEF_MARGIN: f32 = 1.5;
/// So lange bleibt die Hilfe höchstens weg.
const RELIEF_SECONDS: f32 = 240.0;
/// Hilfe kehrt um, wenn weniger als dieser Anteil übrig ist.
const RELIEF_RETREAT: f32 = 0.4;
/// So oft schickt er einem bedrängten Verbündeten Waren.
const AID_SECONDS: f32 = 120.0;
/// So viel Essen jeder Sorte behält er.
const AID_FOOD_RESERVE: u32 = 30;
/// So viele Taler behält er mindestens.
const AID_TALER_RESERVE: u32 = 100;

/// Ein Computergegner in einem laufenden Spiel.
#[derive(Debug, Clone)]
pub struct Computer {
    pub player: u32,
    pub seat: Seat,
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
    /// Zu welchem Abbaubetrieb er zuletzt einen Weg gelegt hat.
    road_target: usize,
    /// Welle unterwegs.
    campaign: Option<Campaign>,
    /// Wann er wieder prüft, ob die Burg dicht ist.
    next_seal: f32,
    /// Grundriss, einmal gerechnet (für die Engstelle zählt der Wald zu Beginn).
    layout: Option<Layout>,
    /// Frühestens dann die nächste Welle.
    next_attack: f32,
    /// Seit wann eine Welle bereitsteht, die er für zu schwach hält.
    weak_since: Option<f32>,
    /// Truppen, die einem Verbündeten helfen.
    relief: Option<Relief>,
    /// Frühestens dann wieder Waren an einen bedrängten Verbündeten.
    next_aid: f32,
}

impl Computer {
    pub fn new(seat: Seat, personalities: &AiPersonalities, economy: &Economy) -> Computer {
        let profile = Profile::for_seat(&seat, personalities, economy);
        let next_attack = profile.attack.as_ref().map_or(f32::MAX, |a| a.first_minutes * 60.0);
        Computer {
            player: seat.player,
            // Leicht versetzt, damit nicht alle Computergegner im selben Tick rechnen.
            next_think: 1.0 + (seat.player % 4) as f32 * 0.5,
            seat,
            profile,
            clock: 0.0,
            skip_until: HashMap::new(),
            gate_skip: HashMap::new(),
            next_unit: 0,
            road_target: 0,
            campaign: None,
            weak_since: None,
            relief: None,
            next_aid: 0.0,
            next_attack,
            layout: None,
            next_seal: 0.0,
        }
    }

    /// Nur mit Stärke, Charakter wie die Stufe (Tests).
    #[cfg(test)]
    pub fn with_level(player: u32, level: AiLevel, economy: &Economy) -> Computer {
        let all = AiPersonalities { personalities: vec![personality::neutral()] };
        Computer::new(Seat { player, level, personality: String::new(), daring: None, helpfulness: None }, &all, economy)
    }

    /// Charaktere wurden im Editor geändert: Profil neu, alles andere bleibt.
    pub fn reconfigure(&mut self, personalities: &AiPersonalities, economy: &Economy) {
        self.layout = None;
        self.profile = Profile::for_seat(&self.seat, personalities, economy);
        if self.profile.attack.is_none() {
            self.next_attack = f32::MAX;
        } else if self.next_attack == f32::MAX {
            self.next_attack = self.clock + 60.0;
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
        let layout = match self.layout.take() {
            Some(l) => l,
            None => Layout::new(world, keep, self.profile.wall_gap, self.profile.main_gate, self.profile.castle),
        };
        let cmds = self.think_with(world, &layout);
        self.layout = Some(layout);
        cmds
    }

    fn think_with(&mut self, world: &World, layout: &Layout) -> Vec<ClientMsg> {
        let stock = world.stock_state(self.player);
        let mut cmds = Vec::new();
        // Erst die Bauliste; ist sie durch, der Mauerring, dann Wege, dann Ausbauten.
        // (Mauer und Wege früher bremsen die Wirtschaft zu sehr, besonders bei wenig Holz.)
        let next = self.build_next(world, layout, &stock, &mut cmds);
        if let Next::Idle = next {
            // Mauer und Wege teilen sich, was über der Reserve liegt.
            let mut budget = Budget::new(&stock, self.profile.wall_wood_reserve);
            let before = cmds.len();
            // Pflaster zuerst (wenn das Profil es will): kostet nur Stein, die Mauer braucht Blöcke.
            // Aber erst, wenn ein Tor steht: das braucht Holz und Stein, die Wege auch.
            let roads_first = self.profile.roads_first && self.count(world, BuildingKind::Gate) + self.count(world, BuildingKind::Gatehouse) > 0;
            if roads_first {
                self.build_roads(world, layout, &mut budget, true, &mut cmds);
            }
            let walls_pending = self.build_walls(world, layout, &mut budget, &mut cmds);
            // Wartet die Mauer auf Material (meist Bretter fürs Tor), keine Wege: die
            // brauchen dasselbe Holz.
            if !roads_first && !walls_pending {
                self.build_roads(world, layout, &mut budget, true, &mut cmds);
            }
            if cmds.len() == before && !walls_pending {
                self.upgrade(world, &stock, &mut cmds);
            }
        }
        self.configure(world, &stock, &mut cmds);
        let missing = match next {
            Next::Waiting(missing) => missing,
            // Bauliste durch, aber noch kein Tor (etwa weil Stein fehlt) oder das Torhaus fehlt:
            // dafür zukaufen.
            Next::Idle if self.count(world, BuildingKind::Barracks) > 0 => {
                if self.count(world, BuildingKind::Gate) + self.count(world, BuildingKind::Gatehouse) == 0 {
                    shortfall(world, &stock, BuildingKind::Gate)
                } else if layout.gates().iter().any(|g| g.0 == BuildingKind::Gatehouse) && self.count(world, BuildingKind::Gatehouse) == 0 {
                    shortfall(world, &stock, BuildingKind::Gatehouse)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        self.trade(world, &stock, &missing, &mut cmds);
        self.train(world, &stock, &mut cmds);
        self.guard_king(world, &mut cmds);
        let defending = self.defend(world, &mut cmds);
        if defending {
            // Daheim brennt es: wer beim Verbündeten hilft, kommt mit zur Verteidigung.
            self.relief = None;
        } else {
            self.help_allies(world, layout, &mut cmds);
            self.attack(world, layout, &mut cmds);
        }
        self.aid(world, &stock, &mut cmds);
        self.rally(world, layout, &mut cmds);
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
    fn build_next(&mut self, world: &World, layout: &Layout, stock: &StockState, cmds: &mut Vec<ClientMsg>) -> Next {
        // Fehlt etwas, das er (noch) nicht herstellen und nicht kaufen kann, nimmt er das
        // nächste Gebäude der Liste, statt ewig zu warten.
        let (kind, near) = loop {
            let Some((kind, near)) = self.next_goal(world, stock) else { return Next::Idle };
            if self.affordable(world, stock, kind) {
                break (kind, near);
            }
            let missing = shortfall(world, stock, kind);
            if missing.iter().all(|g| self.can_get(world, g.good)) {
                return Next::Waiting(missing);
            }
            debug!("Computer {}: {kind:?} braucht {missing:?}, kann er nicht herstellen", self.player);
            self.skip_until.insert(kind, self.clock + RETRY_SECONDS);
        };
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
        Next::Placed
    }

    /// Kann er an diese Ware kommen: ein eigener Betrieb stellt sie her (auf seiner
    /// jetzigen Stufe), oder er hat einen Markt und handelt?
    fn can_get(&self, world: &World, good: Good) -> bool {
        if self.profile.trade.is_some() && self.count(world, BuildingKind::Market) > 0 {
            return true;
        }
        world.buildings.iter().filter(|b| b.owner == self.player).any(|b| {
            world.economy.building(b.kind).is_some_and(|d| {
                d.levels.iter().take(b.level as usize).flat_map(|l| &l.recipes).any(|id| {
                    world.economy.recipe(id).is_some_and(|r| r.outputs.iter().any(|o| o.good == good))
                })
            })
        })
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
        if people.fed < people.working && orchards > 0 && orchards < people.working / 5 + 1 && ready(BuildingKind::Orchard) {
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
        // Viel Holz oder Stein in einem Lager weit draußen: die Werkstatt dazu dorthin, sonst
        // holt ihr Arbeiter jede Ladung quer über die Karte.
        if let Some(goal) = self.workshop_near_input(world, &ready) {
            return Some(goal);
        }
        // Lager fast voll: noch ein Vorratslager.
        // (Nur Bergfried und Vorratslager zählen: ein voller Kornspeicher wird so nicht leerer.)
        let general = |p: &&protocol::StoragePool| matches!(p.kind, BuildingKind::Keep | BuildingKind::Storehouse);
        let (used, cap) = stock.pools.iter().filter(general).fold((0, 0), |(u, c), p| (u + p.used, c + p.capacity));
        let full = cap > 0 && used * 10 >= cap * 9;
        if full && self.count(world, BuildingKind::Storehouse) < 4 && ready(BuildingKind::Storehouse) {
            return Some((BuildingKind::Storehouse, None));
        }
        self.profile
            .build_order
            .iter()
            .find(|&&(k, n)| self.count(world, k) < n && ready(k))
            .map(|&(k, _)| (k, None))
    }

    /// Werkstatt (Sägewerk, Steinmetz), die schon einmal steht, noch einmal neben ein Lager weit
    /// draußen, in dem viel von ihrem Eingang liegt.
    fn workshop_near_input(&self, world: &World, ready: &dyn Fn(BuildingKind) -> bool) -> Option<(BuildingKind, Option<(i64, i64)>)> {
        let center = |b: &protocol::BuildingState| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let own = || world.buildings.iter().filter(|b| b.owner == self.player);
        let keep = own().find(|b| b.kind == BuildingKind::Keep).map(center)?;
        for (kind, input) in [(BuildingKind::Sawmill, Good::Wood), (BuildingKind::Stonemason, Good::Stone)] {
            if self.count(world, kind) == 0 || !ready(kind) {
                continue;
            }
            let workshops: Vec<(f32, f32)> = own().filter(|b| b.kind == kind).map(center).collect();
            let hub = own().filter(|b| b.kind == BuildingKind::Storehouse).find(|b| {
                let c = center(b);
                let far = (c.0 - keep.0).hypot(c.1 - keep.1) > HUB_DISTANCE;
                let lots = b.inventory.iter().any(|g| g.good == input && g.amount >= REMOTE_INPUT);
                let served = workshops.iter().any(|w| (w.0 - c.0).hypot(w.1 - c.1) <= HUB_DISTANCE);
                far && lots && !served
            });
            if let Some(h) = hub {
                let c = center(h);
                return Some((kind, Some((c.0 as i64, c.1 as i64))));
            }
        }
        None
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
    fn configure(&self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        for b in world.buildings.iter().filter(|b| b.owner == self.player) {
            let Some(current) = b.settings.recipe.as_deref().and_then(|r| world.economy.recipe(r)) else { continue };
            // Rezept wechseln: unter den freigeschalteten, die er will (und dem laufenden),
            // das, wovon am wenigsten im Vorrat liegt.
            let unlocked: Vec<&str> = world
                .economy
                .building(b.kind)
                .map(|d| d.levels.iter().take(b.level as usize).flat_map(|l| l.recipes.iter().map(String::as_str)).collect())
                .unwrap_or_default();
            let low = |id: &str| world.economy.recipe(id).and_then(|r| r.outputs.first()).map_or(u32::MAX, |o| have(stock, o.good));
            let best = unlocked
                .iter()
                .copied()
                .filter(|id| *id == current.id || self.profile.recipes.iter().any(|r| r == id))
                .min_by_key(|id| low(id))
                .unwrap_or(&current.id);
            // Erst wechseln, wenn der Unterschied deutlich ist, sonst klappert es hin und her.
            let switch = best != current.id && low(best) + 4 < low(&current.id);
            let recipe = if switch { world.economy.recipe(best).unwrap_or(current) } else { current };
            let stop_at: Vec<GoodAmount> = recipe
                .outputs
                .iter()
                .filter_map(|o| self.profile.stock_targets.iter().find(|t| t.0 == o.good))
                .map(|&(good, amount)| GoodAmount { good, amount })
                .collect();
            if switch || (b.settings.stop_at.is_empty() && !stop_at.is_empty()) {
                let mut settings = b.settings.clone();
                settings.recipe = Some(recipe.id.clone());
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
    /// Steht die Mauer, kommen (je nach Profil) Ecktürme dazu.
    /// `true`, solange am Ring noch etwas zu tun ist (dann wartet er mit Ausbauten).
    fn build_walls(&mut self, world: &World, layout: &Layout, budget: &mut Budget, cmds: &mut Vec<ClientMsg>) -> bool {
        if !self.profile.walls || self.count(world, BuildingKind::Barracks) == 0 {
            return false;
        }
        let p = self.player;
        // Tore zuerst, sonst sperrt er sich selbst ein. Fehlt für eins das Material (etwa
        // Eisen fürs Torhaus), bleibt dort die Lücke frei und es geht mit den anderen weiter.
        let gates = layout.gates();
        let own_gate = |g: &GateSpot| {
            world.buildings.iter().any(|b| b.owner == p && b.kind == g.0 && b.x == g.1 && b.y == g.2)
        };
        let mut waiting = false;
        for (i, g) in gates.iter().enumerate() {
            let &(kind, x, y, rotation) = g;
            if own_gate(g) || self.gate_skip.get(&i).is_some_and(|&t| self.clock < t) {
                continue;
            }
            if !budget.can_pay(world, kind) {
                waiting = true;
                continue;
            }
            if world.check_placement(p, kind, (x, y), rotation, 1).is_err() {
                self.gate_skip.insert(i, self.clock + RETRY_SECONDS);
                continue;
            }
            budget.pay(world, kind);
            cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation, level: 1 });
            return true;
        }
        // Ohne ein einziges Tor keine Mauer.
        if !gates.iter().any(own_gate) {
            return waiting;
        }
        let kind = self.profile.wall_kind;
        let mut placed = 0;
        for (x, y) in layout.ring_tiles() {
            if x < 0 || y < 0 || layout.is_gate_tile(world, x, y) {
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            // Schon eine Mauer (oder ein anderes Gebäude) dort; einen Weg ersetzt die Mauer.
            let taken = world.buildings.iter().any(|b| {
                x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h && !is_road(world, b.kind)
            });
            if taken {
                continue;
            }
            if placed >= self.profile.walls_per_think || !budget.can_pay(world, kind) {
                return true;
            }
            if world.check_placement(p, kind, (x, y), 0, 1).is_err() {
                continue;
            }
            budget.pay(world, kind);
            cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation: 0, level: 1 });
            placed += 1;
        }
        if placed > 0 {
            return true;
        }
        // Linie fertig, aber trotzdem ein Weg hinein (etwa weil Wald an der Engstelle gefällt
        // wurde)? Die Lücke zumauern, ein Stück je Prüfung.
        if self.clock >= self.next_seal {
            self.next_seal = self.clock + SEAL_SECONDS;
            let st = world.castle_status(p);
            if let (false, Some((x, y))) = (st.closed, st.gap) {
                if !layout.is_gate_tile(world, x as i64, y as i64) && budget.can_pay(world, kind) && world.check_placement(p, kind, (x, y), 0, 1).is_ok() {
                    budget.pay(world, kind);
                    cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation: 0, level: 1 });
                    // Gleich noch einmal nachsehen.
                    self.next_seal = self.clock;
                    return true;
                }
            }
        }
        // Ring fertig: Ecktürme (stehen auf der Mauer, die Mauerstücke darunter weichen).
        if self.profile.corner_towers {
            for (x, y) in layout.corner_towers() {
                if x < 0 || y < 0 {
                    continue;
                }
                let (x, y) = (x as u32, y as u32);
                let have_tower = world.buildings.iter().any(|b| b.owner == p && b.kind == BuildingKind::Tower && b.x == x && b.y == y);
                if have_tower {
                    continue;
                }
                if !budget.can_pay(world, BuildingKind::Tower) {
                    return true;
                }
                if world.check_placement(p, BuildingKind::Tower, (x, y), 0, 1).is_ok() {
                    budget.pay(world, BuildingKind::Tower);
                    cmds.push(ClientMsg::PlaceBuilding { kind: BuildingKind::Tower, x, y, rotation: 0, level: 1 });
                    return true;
                }
            }
        }
        waiting
    }

    /// Wege: erst die Straßen vom Bergfried zu den Toren, dann (je nach Profil) vom Tor hinaus
    /// zu den Abbaubetrieben, so weit der Bauradius reicht. Läuft neben allem anderen her,
    /// sobald ein Sägewerk steht, und nur mit dem, was über der Holzreserve liegt.
    /// `wood_ok`: Holzweg als Ersatz für Pflaster erlaubt (nicht, solange die Mauer Holz braucht).
    fn build_roads(&mut self, world: &World, layout: &Layout, budget: &mut Budget, wood_ok: bool, cmds: &mut Vec<ClientMsg>) {
        let Some(preferred) = self.profile.road_kind else { return };
        // Erst wenn die Wirtschaft läuft (Kaserne steht), vorher braucht er alles für Betriebe.
        if self.count(world, BuildingKind::Barracks) == 0 {
            return;
        }
        let p = self.player;
        // Pflaster nur vom Überschuss: Stein braucht er auch für Mauer und Tore.
        budget.stone_reserve = ROAD_STONE_RESERVE;
        let mut tiles = layout.street_tiles(self.profile.roads_all_sides);
        if self.profile.roads_to_producers {
            tiles.extend(self.producer_road(world, layout));
        }
        let mut placed = 0;
        for (x, y) in tiles {
            // Fehlt der Stein fürs Pflaster, tut es auch ein Holzweg.
            let fallback = if wood_ok { BuildingKind::WoodPath } else { preferred };
            let Some(kind) = [preferred, fallback].into_iter().find(|&k| budget.can_pay(world, k)) else {
                return;
            };
            if placed >= self.profile.roads_per_think {
                return;
            }
            // Nicht auf die Ringlinie (dort kommt die Mauer hin), außer unter ein Tor.
            if x < 0 || y < 0 || (layout.on_ring(x, y) && !layout.is_gate_tile(world, x, y)) {
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            match world.buildings.iter().find(|b| x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h) {
                None => {
                    if world.check_placement(p, kind, (x, y), 0, 1).is_err() {
                        continue;
                    }
                }
                // Weg durch den Durchgang eines eigenen Tores: macht ihn schneller.
                Some(b) if b.owner == p && is_gate(b.kind) && b.road_under == 0 && in_passage(b, x, y) => {}
                Some(_) => continue,
            }
            budget.pay(world, kind);
            cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation: 0, level: 1 });
            placed += 1;
        }
    }

    /// Weg vom nächsten eigenen Tor zu einem Abbaubetrieb draußen (jedes Mal ein anderer).
    fn producer_road(&mut self, world: &World, layout: &Layout) -> Vec<(i64, i64)> {
        let p = self.player;
        let gates: Vec<GateSpot> = layout
            .gates()
            .into_iter()
            .filter(|g| world.buildings.iter().any(|b| b.owner == p && b.kind == g.0 && b.x == g.1 && b.y == g.2))
            .collect();
        let producers: Vec<(u32, u32)> = world
            .buildings
            .iter()
            .filter(|b| b.owner == p)
            .filter(|b| world.economy.building(b.kind).is_some_and(|d| d.category == BuildingCategory::Raw))
            .filter_map(|b| b.entrance)
            .collect();
        if gates.is_empty() || producers.is_empty() {
            return Vec::new();
        }
        self.road_target = (self.road_target + 1) % producers.len();
        let goal = producers[self.road_target];
        let start = gates
            .iter()
            .map(|g| layout.outside_of(world, g))
            .min_by_key(|&(x, y)| (x - goal.0 as i64).abs() + (y - goal.1 as i64).abs())
            .expect("mindestens ein Tor");
        let mut path = vec![start];
        path.extend(
            world
                .map
                .nav
                .find_path((start.0 as i32, start.1 as i32), (goal.0 as i32, goal.1 as i32))
                .into_iter()
                .map(|(x, y)| (x as i64, y as i64)),
        );
        path
    }

    /// Am Markt: Überschuss verkaufen, Fehlendes kaufen (nur über der Taler-Reserve).
    /// `missing`: was dem nächsten Gebäude der Bauliste fehlt; das kauft er zu, wenn das
    /// Profil es erlaubt.
    fn trade(&self, world: &World, stock: &StockState, missing: &[GoodAmount], cmds: &mut Vec<ClientMsg>) {
        let Some(trade) = &self.profile.trade else { return };
        if self.count(world, BuildingKind::Market) == 0 {
            return;
        }
        let price = |good: Good| world.economy.def.market.prices.iter().find(|p| p.good == good).copied();
        let mut taler = stock.taler;
        for &(good, limit) in &trade.sell_above {
            let n = have(stock, good).saturating_sub(limit);
            if n > 0 && price(good).is_some() {
                cmds.push(ClientMsg::Trade { good, amount: n, sell: true });
                taler += price(good).map_or(0, |p| p.sell * n);
            }
        }
        if trade.buy_missing {
            for m in missing {
                let Some(pr) = price(m.good) else { continue };
                let cost = pr.buy * m.amount;
                if taler >= cost + trade.taler_reserve {
                    cmds.push(ClientMsg::Trade { good: m.good, amount: m.amount, sell: false });
                    taler -= cost;
                }
            }
        }
        for &(good, target, needed_by) in &trade.buy_up_to {
            let Some(pr) = price(good) else { continue };
            if self.count(world, needed_by) == 0 {
                continue;
            }
            let want = target.saturating_sub(have(stock, good));
            let affordable = taler.saturating_sub(trade.taler_reserve) / pr.buy.max(1);
            let n = want.min(affordable);
            if n > 0 {
                cmds.push(ClientMsg::Trade { good, amount: n, sell: false });
                taler -= n * pr.buy;
            }
        }
    }

    /// Soldaten in Auftrag geben, bis die Armee groß genug ist. Jede Kaserne bekommt bis zu
    /// `queue_per_barracks` Aufträge in ihre Warteschlange.
    fn train(&mut self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        let units = self.profile.army_units.clone();
        if units.is_empty() {
            return;
        }
        let barracks: Vec<_> = world.buildings.iter().filter(|b| b.owner == self.player && b.kind == BuildingKind::Barracks).collect();
        let mut queued: u32 = barracks.iter().map(|b| b.training.len() as u32).sum();
        let queue_max = (world.economy.def.army.queue_max).min(self.profile.queue_per_barracks) as usize;
        // Mit Markt bleibt ein Rest Taler für den Handel liegen.
        let floor = self.profile.trade.as_ref().map_or(0, |t| t.taler_reserve);
        let mut taler = stock.taler.saturating_sub(floor);
        let mut goods: HashMap<Good, u32> = stock.goods.iter().map(|g| (g.good, g.amount)).collect();
        // Freie Pferde in den eigenen Pferdezuchten (Ritter brauchen eins).
        let mut horses = world.horses_available(self.player);
        for b in barracks {
            let mut len = b.training.len();
            while len < queue_max && stock.army.soldiers + queued < self.profile.army_size {
                // Der Reihe nach; kann er den, der dran ist, nicht ausbilden, den nächsten versuchen.
                let pick = (0..units.len()).find_map(|step| {
                    let kind = units[(self.next_unit + step) % units.len()];
                    let def = world.economy.unit(kind)?;
                    let can = world.economy.building(b.kind).is_some_and(|d| {
                        d.levels.iter().take(b.level as usize).any(|l| l.trains.contains(&kind))
                    });
                    let paid = taler >= def.taler && def.cost.iter().all(|c| goods.get(&c.good).copied().unwrap_or(0) >= c.amount);
                    let horse = !def.mounted || horses > 0;
                    (can && paid && horse).then_some((step, kind, def))
                });
                let Some((step, kind, def)) = pick else { break };
                taler -= def.taler;
                for c in &def.cost {
                    *goods.entry(c.good).or_default() -= c.amount;
                }
                horses -= def.mounted as u32;
                cmds.push(ClientMsg::TrainUnits { building_id: b.id, kind, count: 1, spread: false });
                self.next_unit = (self.next_unit + step + 1) % units.len();
                len += 1;
                queued += 1;
            }
        }
        // Übrige Pferde: wartende Soldaten, die reiten können, steigen auf (schneller, Pferd fängt Treffer ab).
        if horses > 0 {
            let riders: Vec<u32> = world
                .units
                .iter()
                .filter(|u| u.owner == self.player && !u.is_moving() && !u.mounted() && !self.on_campaign(u.id))
                .filter(|u| world.economy.unit(u.kind).is_some_and(|d| d.can_ride))
                .take(horses as usize)
                .map(|u| u.id)
                .collect();
            if !riders.is_empty() {
                cmds.push(ClientMsg::Mount { unit_ids: riders });
            }
        }
    }

    /// Wellen: sammeln, Gegner einschätzen, losschicken, unterwegs nachsteuern, heimholen.
    fn attack(&mut self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        let Some(a) = self.profile.attack.clone() else {
            self.campaign = None;
            return;
        };
        let p = self.player;
        if let Some(c) = &mut self.campaign {
            let alive: Vec<&crate::world::Unit> = world.units.iter().filter(|u| u.owner == p && c.units.contains(&u.id)).collect();
            c.units = alive.iter().map(|u| u.id).collect();
            let power: f32 = alive.iter().map(|u| attack::unit_power(world, u.kind)).sum();
            let target_gone = !world.buildings.iter().any(|b| b.owner == c.target && b.kind == BuildingKind::Keep);
            let all_stopped = alive.iter().all(|u| !u.is_moving());
            if all_stopped && c.arrived.is_none() {
                c.arrived = Some(self.clock);
            }
            let siege_over = c.arrived.is_some_and(|t| self.clock - t > a.siege_seconds);
            if alive.is_empty() || target_gone || siege_over || power < c.start_power * a.retreat_below {
                debug!("Computer {p}: Welle gegen {} kehrt um", c.target);
                if let Some(home) = rally_point(world, p, layout) {
                    if !c.units.is_empty() {
                        cmds.push(ClientMsg::MoveUnits { unit_ids: c.units.clone(), x: home.0, y: home.1 });
                    }
                }
                self.campaign = None;
                self.next_attack = self.clock + a.cooldown_seconds;
                return;
            }
            // Wer unterwegs hängen geblieben ist (weit vom Ziel, steht), läuft weiter.
            if c.arrived.is_none() {
                let stuck: Vec<u32> = alive
                    .iter()
                    .filter(|u| !u.is_moving() && (u.pos.0 - c.goal.0).hypot(u.pos.1 - c.goal.1) > 8.0)
                    .map(|u| u.id)
                    .collect();
                if !stuck.is_empty() {
                    cmds.push(ClientMsg::MoveUnits { unit_ids: stuck, x: c.goal.0, y: c.goal.1 });
                }
            }
            return;
        }
        if self.clock < self.next_attack {
            return;
        }
        // Welle: stehende Soldaten am Boden; die dem Bergfried nächsten bleiben als Wache.
        let Some(home) = rally_point(world, p, layout) else { return };
        let mut ready: Vec<&crate::world::Unit> = world
            .units
            .iter()
            .filter(|u| u.owner == p && u.kind.is_soldier() && !u.is_moving() && !u.is_on_wall() && !self.on_relief(u.id))
            .collect();
        ready.sort_by(|x, y| {
            let d = |u: &crate::world::Unit| (u.pos.0 - home.0).hypot(u.pos.1 - home.1);
            d(x).total_cmp(&d(y))
        });
        let wave: Vec<&crate::world::Unit> = ready.into_iter().skip(a.home_guard as usize).collect();
        // Später wieder nachsehen (nicht jede Überlegung die ganze Karte absuchen).
        self.next_attack = self.clock + 30.0;
        if (wave.len() as u32) < a.wave_min {
            self.weak_since = None;
            return;
        }
        let power: f32 = wave.iter().map(|u| attack::unit_power(world, u.kind)).sum();
        let Some((target, enemy, goal)) = attack::choose_target(world, p, wave[0].pos) else { return };
        // Mut: je länger die Welle schon bereitsteht, desto eher zieht sie auch gegen einen
        // stärkeren Gegner los (bei vollem Mut sofort).
        let since = *self.weak_since.get_or_insert(self.clock);
        let waited = (self.clock - since) / a.cooldown_seconds.max(1.0);
        let needed = attack::needed_ratio(a.superiority, self.profile.daring, waited);
        if power < enemy * needed {
            debug!("Computer {p}: Welle {power:.0} zu schwach gegen Spieler {target} ({enemy:.0}, braucht ×{needed:.2})");
            return;
        }
        self.weak_since = None;
        debug!("Computer {p}: greift Spieler {target} an ({} Soldaten, {power:.0} gegen {enemy:.0})", wave.len());
        let units: Vec<u32> = wave.iter().map(|u| u.id).collect();
        cmds.push(ClientMsg::MoveUnits { unit_ids: units.clone(), x: goal.0, y: goal.1 });
        self.campaign = Some(Campaign { target, units, start_power: power, goal, arrived: None });
    }

    /// Der König steht oben auf dem Bergfried: dort kommt kein Nahkämpfer vom Boden an ihn
    /// heran, und Fernkämpfer können ihm nichts. (Darf jeder Spieler genauso.)
    fn guard_king(&self, world: &World, cmds: &mut Vec<ClientMsg>) {
        let p = self.player;
        let Some(king) = world.units.iter().find(|u| u.owner == p && u.kind == UnitKind::King) else { return };
        if king.is_on_wall() || king.is_moving() {
            return;
        }
        let Some(keep) = world.buildings.iter().find(|b| b.owner == p && b.kind == BuildingKind::Keep) else { return };
        if world.economy.building(BuildingKind::Keep).is_none_or(|d| d.garrison == 0) {
            return;
        }
        let c = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        cmds.push(ClientMsg::MoveUnits { unit_ids: vec![king.id], x: c.0, y: c.1 });
    }

    /// Feinde nahe am Bergfried: alle Soldaten am Boden, die nicht mit einer Welle unterwegs
    /// sind, gehen auf den, der dem Bergfried am nächsten ist. `true`, solange er sich wehrt
    /// (dann schickt er keine neue Welle los).
    fn defend(&self, world: &World, cmds: &mut Vec<ClientMsg>) -> bool {
        let p = self.player;
        let Some(keep) = world.buildings.iter().find(|b| b.owner == p && b.kind == BuildingKind::Keep) else { return false };
        let c = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        let dist = |u: &crate::world::Unit| (u.pos.0 - c.0).hypot(u.pos.1 - c.1);
        let threat = world
            .units
            .iter()
            .filter(|u| u.kind.is_soldier() && u.owner != p && world.can_attack(p, u.owner) && dist(u) < THREAT_RADIUS)
            .min_by(|a, b| dist(a).total_cmp(&dist(b)));
        let Some(threat) = threat else { return false };
        let defenders: Vec<u32> = world
            .units
            .iter()
            .filter(|u| u.owner == p && u.kind.is_soldier() && !u.is_on_wall() && !self.on_campaign(u.id))
            // Wer schon nah dran ist, kämpft von selbst.
            .filter(|u| (u.pos.0 - threat.pos.0).hypot(u.pos.1 - threat.pos.1) > 2.0)
            .map(|u| u.id)
            .collect();
        if !defenders.is_empty() {
            debug!("Computer {p}: verteidigt gegen Spieler {}", threat.owner);
            cmds.push(ClientMsg::MoveUnits { unit_ids: defenders, x: threat.pos.0, y: threat.pos.1 });
        }
        true
    }

    /// Ist dieser Soldat gerade mit einer Welle unterwegs?
    fn on_campaign(&self, id: u32) -> bool {
        self.campaign.as_ref().is_some_and(|c| c.units.contains(&id))
    }

    /// Hilft dieser Soldat gerade einem Verbündeten?
    fn on_relief(&self, id: u32) -> bool {
        self.relief.as_ref().is_some_and(|r| r.units.contains(&id))
    }

    /// Verbündeter in Not: einen Teil der freien Soldaten hinschicken. Nur so viele, wie
    /// gegen die Angreifer nötig sind (anderthalbfach), höchstens `helpfulness` der freien
    /// und nie die Wache daheim: sonst lockt ein Scheinangriff die ganze Armee weg.
    fn help_allies(&mut self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        let p = self.player;
        if let Some(r) = &mut self.relief {
            let alive: Vec<&crate::world::Unit> = world.units.iter().filter(|u| u.owner == p && r.units.contains(&u.id)).collect();
            r.units = alive.iter().map(|u| u.id).collect();
            let power: f32 = alive.iter().map(|u| attack::unit_power(world, u.kind)).sum();
            let enemies = attack::threats(world, r.ally, p, THREAT_RADIUS);
            let done = alive.is_empty() || enemies.is_empty() || world.is_eliminated(r.ally) || self.clock > r.until;
            if done || power < r.start_power * RELIEF_RETREAT {
                debug!("Computer {p}: Hilfe für Spieler {} kehrt heim", r.ally);
                if let (Some(home), false) = (rally_point(world, p, layout), r.units.is_empty()) {
                    cmds.push(ClientMsg::MoveUnits { unit_ids: r.units.clone(), x: home.0, y: home.1 });
                }
                self.relief = None;
                return;
            }
            // Wer steht und keinen Feind nah hat, geht auf den nächsten Angreifer.
            let target = enemies[0].pos;
            let idle: Vec<u32> = alive
                .iter()
                .filter(|u| !u.is_moving() && enemies.iter().all(|e| (e.pos.0 - u.pos.0).hypot(e.pos.1 - u.pos.1) > 6.0))
                .map(|u| u.id)
                .collect();
            if !idle.is_empty() {
                cmds.push(ClientMsg::MoveUnits { unit_ids: idle, x: target.0, y: target.1 });
            }
            return;
        }
        if self.profile.helpfulness <= 0.0 {
            return;
        }
        // Der Verbündete, dem es am schlimmsten geht.
        let mut worst: Option<(u32, f32, (f32, f32))> = None;
        for ally in world.players() {
            if ally == p || !world.allied(p, ally) || world.is_eliminated(ally) {
                continue;
            }
            let enemies = attack::threats(world, ally, p, THREAT_RADIUS);
            let power: f32 = enemies.iter().map(|u| attack::unit_power(world, u.kind)).sum();
            if power > 0.0 && worst.is_none_or(|w| power > w.1) {
                worst = Some((ally, power, enemies[0].pos));
            }
        }
        let Some((ally, threat, at)) = worst else { return };
        let Some(home) = rally_point(world, p, layout) else { return };
        let guard = self.profile.attack.as_ref().map_or(2, |a| a.home_guard) as usize;
        let mut free: Vec<&crate::world::Unit> = world
            .units
            .iter()
            .filter(|u| u.owner == p && u.kind.is_soldier() && !u.is_on_wall() && !self.on_campaign(u.id))
            .collect();
        // Die nächsten am Sammelpunkt bleiben als Wache, geschickt werden die dem Verbündeten nächsten.
        free.sort_by(|x, y| {
            let d = |u: &crate::world::Unit| (u.pos.0 - home.0).hypot(u.pos.1 - home.1);
            d(x).total_cmp(&d(y))
        });
        let mut free: Vec<&crate::world::Unit> = free.into_iter().skip(guard).collect();
        let cap = (free.len() as f32 * self.profile.helpfulness).floor() as usize;
        if cap < 2 {
            return;
        }
        free.sort_by(|x, y| {
            let d = |u: &crate::world::Unit| (u.pos.0 - at.0).hypot(u.pos.1 - at.1);
            d(x).total_cmp(&d(y))
        });
        let mut units = Vec::new();
        let mut power = 0.0;
        for u in free.into_iter().take(cap) {
            if power >= threat * RELIEF_MARGIN {
                break;
            }
            power += attack::unit_power(world, u.kind);
            units.push(u.id);
        }
        debug!("Computer {p}: hilft Spieler {ally} mit {} Soldaten ({power:.0} gegen {threat:.0})", units.len());
        cmds.push(ClientMsg::MoveUnits { unit_ids: units.clone(), x: at.0, y: at.1 });
        self.relief = Some(Relief { ally, units, start_power: power, until: self.clock + RELIEF_SECONDS });
    }

    /// Waren für einen bedrängten Verbündeten: ein Teil (nach Hilfsbereitschaft) der Waffen,
    /// Rüstungen, des Essens über einer Reserve und der Taler über der Reserve.
    fn aid(&mut self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        let p = self.player;
        let h = self.profile.helpfulness;
        if h <= 0.0 || self.clock < self.next_aid {
            return;
        }
        let ally = world
            .players()
            .into_iter()
            .filter(|&a| a != p && world.allied(p, a) && !world.is_eliminated(a))
            .find(|&a| !attack::threats(world, a, p, THREAT_RADIUS).is_empty());
        let Some(ally) = ally else { return };
        let eco = &world.economy.def;
        let mut goods: Vec<GoodAmount> = Vec::new();
        let mut give = |good: Good, keep: u32, share: f32| {
            let n = ((have(stock, good).saturating_sub(keep)) as f32 * share).floor() as u32;
            if n > 0 && !goods.iter().any(|g| g.good == good) {
                goods.push(GoodAmount { good, amount: n });
            }
        };
        for u in eco.units.iter().filter(|u| u.kind.is_soldier()) {
            for c in &u.cost {
                give(c.good, 0, h * 0.5);
            }
        }
        for &f in &eco.population.food {
            give(f, AID_FOOD_RESERVE, h * 0.3);
        }
        let reserve = self.profile.trade.as_ref().map_or(AID_TALER_RESERVE, |t| t.taler_reserve.max(AID_TALER_RESERVE));
        let taler = (stock.taler.saturating_sub(reserve) as f32 * h * 0.3).floor() as u32;
        self.next_aid = self.clock + AID_SECONDS;
        if goods.is_empty() && taler == 0 {
            return;
        }
        debug!("Computer {p}: schickt Spieler {ally} Hilfe ({goods:?}, {taler} Taler)");
        cmds.push(ClientMsg::SendGoods { to: ally, goods, taler });
    }

    /// Stehende Soldaten: Fernkämpfer (falls im Profil) hinauf auf Bergfried und Türme, so
    /// viele dort Platz haben; alle anderen, die weit weg sind, zum Sammelpunkt.
    fn rally(&self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        let Some(point) = rally_point(world, self.player, layout) else { return };
        let p = self.player;
        let ranged = |k: UnitKind| world.economy.unit(k).is_some_and(|d| d.range > 2.0);
        let mut posts: Vec<((f32, f32), usize)> = Vec::new();
        if self.profile.garrison_ranged {
            for b in world.buildings.iter().filter(|b| b.owner == p) {
                let room = world.economy.building(b.kind).map_or(0, |d| d.garrison as usize);
                if room == 0 {
                    continue;
                }
                let inside = |u: &crate::world::Unit| {
                    u.pos.0 >= b.x as f32 && u.pos.1 >= b.y as f32 && u.pos.0 < (b.x + b.w) as f32 && u.pos.1 < (b.y + b.h) as f32
                };
                let up = world.units.iter().filter(|u| u.owner == p && u.is_on_wall() && inside(u)).count();
                if room > up {
                    let c = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
                    // Der Bergfried zuerst, dann die Türme.
                    let entry = (c, room - up);
                    if b.kind == BuildingKind::Keep {
                        posts.insert(0, entry);
                    } else {
                        posts.push(entry);
                    }
                }
            }
        }
        let idle: Vec<&crate::world::Unit> = world
            .units
            .iter()
            .filter(|u| u.owner == p && u.kind.is_soldier() && !u.is_moving() && !u.is_on_wall() && !self.on_campaign(u.id) && !self.on_relief(u.id))
            .collect();
        let mut to_rally = Vec::new();
        for u in idle {
            if ranged(u.kind) {
                if let Some(post) = posts.iter_mut().find(|(_, room)| *room > 0) {
                    post.1 -= 1;
                    cmds.push(ClientMsg::MoveUnits { unit_ids: vec![u.id], x: post.0 .0, y: post.0 .1 });
                    continue;
                }
            }
            if (u.pos.0 - point.0).hypot(u.pos.1 - point.1) > RALLY_SLACK {
                to_rally.push(u.id);
            }
        }
        if !to_rally.is_empty() {
            cmds.push(ClientMsg::MoveUnits { unit_ids: to_rally, x: point.0, y: point.1 });
        }
    }
}

/// Ergebnis von `build_next`.
enum Next {
    /// Hat gebaut (oder für diesmal keinen Platz gefunden).
    Placed,
    /// Wartet auf Material: so viel fehlt noch.
    Waiting(Vec<GoodAmount>),
    /// Nichts mehr auf der Bauliste.
    Idle,
}

/// Was in dieser Runde noch ausgegeben werden kann (Befehle einer Runde verbrauchen nacheinander).
struct Budget {
    left: HashMap<Good, u32>,
    wood_reserve: u32,
    stone_reserve: u32,
}

impl Budget {
    fn new(stock: &StockState, wood_reserve: u32) -> Budget {
        Budget { left: stock.goods.iter().map(|g| (g.good, g.amount)).collect(), wood_reserve, stone_reserve: 0 }
    }

    fn can_pay(&self, world: &World, kind: BuildingKind) -> bool {
        let Some(def) = world.economy.building(kind) else { return false };
        level_cost(def, 1).iter().all(|c| {
            let keep = match c.good {
                Good::Wood => self.wood_reserve,
                Good::Stone => self.stone_reserve,
                _ => 0,
            };
            self.left.get(&c.good).copied().unwrap_or(0) >= c.amount + keep
        })
    }

    fn pay(&mut self, world: &World, kind: BuildingKind) {
        for c in world.economy.building(kind).map(|d| level_cost(d, 1)).unwrap_or_default() {
            *self.left.entry(c.good).or_default() -= c.amount;
        }
    }
}

fn is_gate(kind: BuildingKind) -> bool {
    matches!(kind, BuildingKind::Gate | BuildingKind::Gatehouse)
}

/// Liegt das Feld im Durchgang des Tores (mittlere Spalte bzw. Zeile, wie `world::gate`)?
fn in_passage(b: &protocol::BuildingState, x: u32, y: u32) -> bool {
    if b.w >= b.h {
        x == b.x + b.w / 2
    } else {
        y == b.y + b.h / 2
    }
}

fn is_road(world: &World, kind: BuildingKind) -> bool {
    world.economy.building(kind).is_some_and(|d| d.road_speed_percent > 0)
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

/// Was für die erste Stufe dieses Gebäudes noch fehlt.
fn shortfall(world: &World, stock: &StockState, kind: BuildingKind) -> Vec<GoodAmount> {
    world
        .economy
        .building(kind)
        .map(|d| level_cost(d, 1))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let short = c.amount.saturating_sub(have(stock, c.good));
            (short > 0).then_some(GoodAmount { good: c.good, amount: short })
        })
        .collect()
}

fn have(stock: &StockState, good: Good) -> u32 {
    stock.goods.iter().find(|g| g.good == good).map_or(0, |g| g.amount)
}

/// Die Computergegner eines Spiels aus der Lobby.
pub fn computers(list: &[Seat], personalities: &AiPersonalities, economy: &Economy) -> Vec<Computer> {
    list.iter().map(|s| Computer::new(s.clone(), personalities, economy)).collect()
}

#[cfg(test)]
mod tests;
