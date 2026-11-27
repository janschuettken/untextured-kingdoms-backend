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
mod siege;

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
/// So lange bleibt ein Posten auf Turm oder Wehrgang gesperrt, zu dem kein Weg führte.
const POST_RETRY_SECONDS: f32 = 30.0;
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
/// Über so viel Taler (zur Reserve dazu) kauft er zu, was ihm zuletzt gefehlt hat ...
const SPARE_TALER: u32 = 80;
/// ... höchstens so viel je Ware und Überlegung.
const SPARE_BATCH: u32 = 15;
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
    /// Was der Mauer gerade fehlt (zum Zukaufen).
    wall_need: Vec<GoodAmount>,
    /// Waren, die in dieser Überlegung gefehlt haben.
    short_now: std::collections::HashSet<Good>,
    /// Seit wann eine Ware fehlt.
    short_since: HashMap<Good, f32>,
    /// Ausgebaute Produktion: so viele Betriebe dieser Art will er (über die Bauliste hinaus).
    extra: HashMap<BuildingKind, u32>,
    /// Türme auf dem Ring, die sich nicht bauen ließen (oben links), bis zu dieser Zeit.
    tower_skip: HashMap<(i64, i64), f32>,
    /// Hat der Mauerring noch offene Stücke, für die gerade Material fehlt oder die er gerade setzt?
    ring_busy: bool,
    /// Ist die eigene Burg zu (alle paar Sekunden neu geprüft)?
    closed: bool,
    next_closed_check: f32,
    /// Seit wann eine Welle bereitsteht, die er für zu schwach hält.
    weak_since: Option<f32>,
    /// Truppen, die einem Verbündeten helfen.
    relief: Option<Relief>,
    /// Frühestens dann wieder Waren an einen bedrängten Verbündeten.
    next_aid: f32,
    /// Schützen, die er zuletzt auf einen Posten geschickt hat (Einheit → Posten).
    post_sent: HashMap<u32, (i64, i64)>,
    /// Posten, zu denen kein Weg führte, bis zu dieser Zeit (sonst jede Überlegung eine
    /// teure, vergebliche Wegsuche je Schütze).
    post_skip: HashMap<(i64, i64), f32>,
    /// Belagerung: ist ein Gegner zu (dann Werkstatt, Baumeister, Geräte)?
    siege_wanted: bool,
    next_siege_check: f32,
    /// Zähler für die Stelle der nächsten Baustelle.
    siege_try: u32,
}

impl Computer {
    pub fn new(seat: Seat, personalities: &AiPersonalities, economy: &Economy) -> Computer {
        let profile = Profile::for_seat(&seat, personalities, economy);
        let next_attack = profile.attack.as_ref().map_or(f32::MAX, |a| a.first_minutes * 60.0);
        Computer {
            player: seat.player,
            // Versetzt, damit nicht zwei Computergegner im selben Tick rechnen: der Anteil am
            // Denktakt folgt dem goldenen Schnitt, so liegen auch acht Spieler weit auseinander.
            next_think: 1.0 + profile.think_seconds * (seat.player as f32 * 0.618_034).fract(),
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
            short_now: Default::default(),
            wall_need: Vec::new(),
            short_since: HashMap::new(),
            extra: HashMap::new(),
            tower_skip: HashMap::new(),
            closed: false,
            ring_busy: false,
            next_closed_check: 0.0,
            post_sent: HashMap::new(),
            post_skip: HashMap::new(),
            siege_wanted: false,
            next_siege_check: 0.0,
            siege_try: 0,
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

    /// Würde er in diesem Tick (nach `dt`) nachdenken?
    pub fn due(&self, dt: f32) -> bool {
        self.clock + dt >= self.next_think
    }

    /// Dieser Tick ist schon voll: die Zeit läuft weiter, nachgedacht wird im nächsten Tick.
    pub fn postpone(&mut self, dt: f32) {
        self.clock += dt;
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
        self.short_now.clear();
        self.wall_need.clear();
        if self.clock >= self.next_closed_check {
            self.next_closed_check = self.clock + 5.0;
            self.closed = crate::world::castle_closed(world, self.player);
        }
        // Erst die Bauliste. Der Mauerring kommt nebenher, sobald das Profil es will
        // (`walls_after`), sonst erst, wenn die Bauliste durch ist; dann Wege, dann Ausbauten.
        let next = self.build_next(world, layout, &stock, &mut cmds);
        let idle = matches!(next, Next::Idle);
        let walls_on = idle || self.profile.walls_after.is_some_and(|k| self.count(world, k) > 0);
        if walls_on {
            // Mauer und Wege teilen sich, was über der Reserve liegt.
            let mut budget = Budget::new(&stock, self.profile.wall_wood_reserve);
            match &next {
                // Was eben gebaut wurde, ist schon ausgegeben.
                Next::Placed(Some(kind)) => budget.pay(world, *kind),
                // Das nächste Gebäude der Bauliste bekommt sein Material zuerst (geht die Mauer vor,
                // wartet die Bauliste ohnehin bis auf das Nötigste, siehe `paused`).
                Next::Waiting(kind, _) => budget.pay(world, *kind),
                _ => {}
            }
            let before = cmds.len();
            // Pflaster zuerst (wenn das Profil es will): kostet nur Stein, die Mauer braucht Blöcke.
            // Aber erst, wenn ein Tor steht: das braucht Holz und Stein, die Wege auch.
            let roads_first = self.profile.roads_first && self.gates_built(world) > 0;
            if roads_first && idle {
                self.build_roads(world, layout, &mut budget, true, &mut cmds);
            }
            let walls_pending = self.build_walls(world, layout, &mut budget, &mut cmds);
            if !self.ring_busy {
                self.fortify(world, layout, &mut budget, &mut cmds);
            }
            // Wartet die Mauer auf Material (meist Bretter fürs Tor), keine Wege: die
            // brauchen dasselbe Holz.
            if idle && !roads_first && !walls_pending {
                self.build_roads(world, layout, &mut budget, true, &mut cmds);
            }
            if idle && cmds.len() == before && !walls_pending {
                self.upgrade(world, &stock, &mut cmds);
            }
        }
        self.configure(world, &stock, &mut cmds);
        let mut missing = match &next {
            Next::Waiting(_, missing) => missing.clone(),
            _ => Vec::new(),
        };
        // Mauer dran, aber noch kein Tor (etwa weil Stein fehlt) oder das Torhaus fehlt:
        // dafür zukaufen.
        if walls_on && self.profile.walls {
            if self.gates_built(world) == 0 {
                missing.extend(shortfall(world, &stock, layout.gates().first().map_or(BuildingKind::Gate, |g| g.0)));
            } else if layout.gates().iter().any(|g| g.0 == BuildingKind::Gatehouse) && self.count(world, BuildingKind::Gatehouse) == 0 {
                missing.extend(shortfall(world, &stock, BuildingKind::Gatehouse));
            }
        }
        for m in &missing {
            self.short_now.insert(m.good);
        }
        missing.extend(std::mem::take(&mut self.wall_need));
        self.repair(world, &stock, &mut cmds);
        self.trade(world, &stock, &missing, &mut cmds);
        self.train(world, &stock, &mut cmds);
        self.siege_works(world, &stock, layout, &mut cmds);
        self.guard_king(world, &mut cmds);
        let defending = self.defend(world, layout, &mut cmds);
        if defending {
            // Daheim brennt es: wer beim Verbündeten hilft, kommt mit zur Verteidigung.
            self.relief = None;
        } else {
            self.help_allies(world, layout, &mut cmds);
            self.attack(world, layout, &mut cmds);
        }
        self.aid(world, &stock, &mut cmds);
        self.rally(world, layout, &mut cmds);
        self.track_shortage(world);
        // Steht ein Feind zu nah, lehnt der Server Bauen, Ausbauen und Reparieren ab: gar nicht erst schicken.
        cmds.retain(|c| self.no_enemy_near(world, c));
        cmds
    }

    fn no_enemy_near(&self, world: &World, cmd: &ClientMsg) -> bool {
        let (kind, at) = match cmd {
            ClientMsg::PlaceBuilding { kind, x, y, .. } => {
                let (w, h) = world.economy.building(*kind).map_or((1, 1), |d| (d.w, d.h));
                (*kind, (*x as f32 + w as f32 / 2.0, *y as f32 + h as f32 / 2.0))
            }
            ClientMsg::UpgradeBuilding { building_id } | ClientMsg::RepairBuilding { building_id } => {
                match world.buildings.iter().find(|b| b.id == *building_id) {
                    Some(b) => (b.kind, (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)),
                    None => return true,
                }
            }
            _ => return true,
        };
        world.check_no_enemy(self.player, kind, at).is_ok()
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
            let Some((kind, near)) = self.next_goal(world, stock) else {
                // Bauliste wartet auf die Mauer: nicht als „fertig“ zählen (keine Wege, Ausbauten).
                return if self.paused(world, stock) { Next::Placed(None) } else { Next::Idle };
            };
            if self.affordable(world, stock, kind) {
                break (kind, near);
            }
            let missing = shortfall(world, stock, kind);
            if missing.iter().all(|g| self.can_get(world, g.good)) {
                return Next::Waiting(kind, missing);
            }
            debug!("Computer {}: {kind:?} braucht {missing:?}, kann er nicht herstellen", self.player);
            self.skip_until.insert(kind, self.clock + RETRY_SECONDS);
        };
        match layout::find_site(world, self.player, kind, layout, near) {
            Some((x, y, rotation)) => {
                debug!("Computer {}: baut {kind:?} bei {x},{y}", self.player);
                cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation, level: 1 });
                Next::Placed(Some(kind))
            }
            None => {
                debug!("Computer {}: kein Platz für {kind:?}", self.player);
                self.skip_until.insert(kind, self.clock + RETRY_SECONDS);
                Next::Placed(None)
            }
        }
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
        // Mauer geht vor (`walls_first`): solange die Burg offen ist, wartet die Bauliste;
        // nur bei reichlich Holz geht es nebenher weiter.
        // Ausgebaute Produktion (eine Ware fehlte lange).
        if let Some((&k, _)) = self.extra.iter().find(|&(&k, &n)| self.count(world, k) < n && ready(k)) {
            return Some((k, None));
        }
        // Wartet die Bauliste auf die Mauer, kommen nur Abbaubetriebe (Holz, Stein) und das
        // Nötigste fürs Heer (Kaserne, Waffen) dazwischen.
        let paused = self.paused(world, stock);
        let raw = |k: BuildingKind| {
            world.economy.building(k).is_some_and(|d| matches!(d.category, BuildingCategory::Raw | BuildingCategory::Military))
                || k == BuildingKind::Armory
        };
        self.profile
            .build_order
            .iter()
            .filter(|&&(k, _)| !paused || raw(k))
            .find(|&&(k, n)| self.count(world, k) < n && ready(k))
            .map(|&(k, _)| (k, None))
    }

    /// Wartet die Bauliste, bis die Burg zu ist (`walls_first`)?
    fn paused(&self, world: &World, stock: &StockState) -> bool {
        let started = self.profile.walls && self.profile.walls_after.is_some_and(|k| self.count(world, k) > 0);
        self.profile.walls_first && started && self.ring_busy && !self.closed && have(stock, Good::Wood) < 60
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
            // Mit Feinden in der Nähe darf nicht ausgebaut werden.
            let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
            if next.cost.iter().all(|c| have(stock, c.good) >= c.amount) && world.check_no_enemy(self.player, b.kind, center).is_ok() {
                cmds.push(ClientMsg::UpgradeBuilding { building_id: b.id });
            }
            return;
        }
    }

    /// Mauerring mit Toren, ein paar Stücke pro Überlegung. Erst wenn die Kaserne steht.
    /// Steht die Mauer, kommen (je nach Profil) Ecktürme dazu.
    /// `true`, solange am Ring noch etwas zu tun ist (dann wartet er mit Ausbauten).
    fn build_walls(&mut self, world: &World, layout: &Layout, budget: &mut Budget, cmds: &mut Vec<ClientMsg>) -> bool {
        if !self.profile.walls {
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
                self.short_now.extend(budget.short(world, kind));
                // Das Material dafür hält er zurück, sonst frisst die Mauer es weg.
                budget.pay(world, kind);
                waiting = true;
                continue;
            }
            if let Err(e) = world.check_placement(p, kind, (x, y), rotation, 1) {
                // Feinde in der Nähe (etwa nachdem sie das Tor eingerissen haben): bald wieder
                // versuchen, sonst bleibt die Lücke lange offen.
                let wait = if e.key == "error.enemy_near" { 10.0 } else { RETRY_SECONDS };
                self.gate_skip.insert(i, self.clock + wait);
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
        self.ring_busy = true;
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
            if placed >= self.profile.walls_per_think {
                return true;
            }
            // Reicht es nicht für die richtige Mauer, erst eine schnelle (Palisade): Hauptsache zu.
            // Später ersetzt `fortify` sie durch die richtige.
            let Some(kind) = self.wall_piece(world, budget) else { return true };
            if world.check_placement(p, kind, (x, y), 0, 1).is_err() {
                continue;
            }
            budget.pay(world, kind);
            cmds.push(ClientMsg::PlaceBuilding { kind, x, y, rotation: 0, level: 1 });
            placed += 1;
        }
        self.ring_busy = placed > 0;
        if placed > 0 {
            return true;
        }
        // Linie fertig, aber trotzdem ein Weg hinein (etwa weil Wald an der Engstelle gefällt
        // wurde)? Die Lücke zumauern, ein Stück je Prüfung.
        if self.clock >= self.next_seal {
            self.next_seal = self.clock + SEAL_SECONDS;
            let st = world.castle_status(p);
            let kind = self.wall_piece(world, budget).unwrap_or(kind);
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

    /// Welches Mauerstück er jetzt setzen kann: das richtige, sonst das schnelle. Fehlt für
    /// das richtige etwas, merkt er es sich (fehlt lange: ausbauen; mit Markt: zukaufen).
    fn wall_piece(&mut self, world: &World, budget: &Budget) -> Option<BuildingKind> {
        let kind = self.profile.wall_kind;
        if budget.can_pay(world, kind) {
            return Some(kind);
        }
        let short = budget.short(world, kind);
        self.short_now.extend(short.iter().copied());
        if let Some(cost) = world.economy.building(kind).map(|d| level_cost(d, 1)) {
            for c in cost.into_iter().filter(|c| short.contains(&c.good)) {
                if !self.wall_need.iter().any(|w| w.good == c.good) {
                    self.wall_need.push(GoodAmount { good: c.good, amount: c.amount * 4 });
                }
            }
        }
        self.profile.quick_wall.filter(|&q| budget.can_pay(world, q))
    }

    /// Wie viele Tore (einfach oder Torhaus) er hat.
    fn gates_built(&self, world: &World) -> u32 {
        self.count(world, BuildingKind::Gate) + self.count(world, BuildingKind::Gatehouse)
    }

    /// Ring steht: mehr Türme auf der Mauer (`tower_spacing`), dann Zinnen (`battlements`):
    /// Stufe 2 erst für die Tore (Treppe hinauf), dann Türme, dann die Mauerstücke.
    fn fortify(&mut self, world: &World, layout: &Layout, budget: &mut Budget, cmds: &mut Vec<ClientMsg>) {
        if !self.profile.walls || self.gates_built(world) == 0 {
            return;
        }
        let p = self.player;
        // Schnelle Mauer (Palisade) durch die richtige ersetzen: abreißen und im selben Zug
        // neu setzen, so ist die Lücke nie offen.
        if let Some(quick) = self.profile.quick_wall.filter(|&q| q != self.profile.wall_kind) {
            let kind = self.profile.wall_kind;
            let mut swapped = 0;
            for b in world.buildings.iter().filter(|b| b.owner == p && b.kind == quick) {
                if swapped >= self.profile.walls_per_think / 2 || !budget.can_pay(world, kind) {
                    break;
                }
                if world.enemy_near(p, kind, (b.x as f32 + 0.5, b.y as f32 + 0.5)) {
                    continue;
                }
                budget.pay(world, kind);
                cmds.push(ClientMsg::DemolishBuilding { building_id: b.id });
                cmds.push(ClientMsg::PlaceBuilding { kind, x: b.x, y: b.y, rotation: 0, level: 1 });
                swapped += 1;
            }
        }
        if self.profile.tower_spacing > 0 && self.profile.corner_towers {
            if let Some((x, y)) = self.next_ring_tower(world, layout) {
                if budget.can_pay(world, BuildingKind::Tower) {
                    budget.pay(world, BuildingKind::Tower);
                    cmds.push(ClientMsg::PlaceBuilding { kind: BuildingKind::Tower, x, y, rotation: 0, level: 1 });
                } else {
                    self.short_now.extend(budget.short(world, BuildingKind::Tower));
                }
            }
        }
        if !self.profile.battlements {
            return;
        }
        let rank = |k: BuildingKind| match k {
            BuildingKind::Gate => Some(0),
            BuildingKind::Tower | BuildingKind::HighTower => Some(1),
            BuildingKind::Palisade | BuildingKind::Wall | BuildingKind::HighWall => Some(2),
            _ => None,
        };
        let mut todo: Vec<&protocol::BuildingState> =
            world.buildings.iter().filter(|b| b.owner == p && b.level < 2 && rank(b.kind).is_some()).collect();
        // Erst die Tore, dann Türme, dann Mauern; die Mauer zuerst am Haupttor.
        let main = layout.gates().first().map_or(layout.mid, |g| (g.1 as i64, g.2 as i64));
        todo.sort_by_key(|b| (rank(b.kind), (b.x as i64 - main.0).abs() + (b.y as i64 - main.1).abs()));
        let mut done = 0;
        for b in todo {
            if done >= self.profile.walls_per_think {
                break;
            }
            let Some(cost) = world.economy.building(b.kind).and_then(|d| d.levels.get(b.level as usize)).map(|l| l.cost.clone()) else {
                continue;
            };
            if !budget.take(&cost) {
                self.short_now.extend(budget.short_cost(&cost));
                break;
            }
            cmds.push(ClientMsg::UpgradeBuilding { building_id: b.id });
            done += 1;
        }
    }

    /// Nächster zusätzlicher Turm auf dem Ring: ein Mauerfeld, das von allen Türmen und
    /// Toren mindestens `tower_spacing` Felder weg ist. Liefert die Ecke oben links (3x3).
    fn next_ring_tower(&mut self, world: &World, layout: &Layout) -> Option<(u32, u32)> {
        let p = self.player;
        let spacing = self.profile.tower_spacing as i64;
        let mut centers: Vec<(i64, i64)> = world
            .buildings
            .iter()
            .filter(|b| b.owner == p && matches!(b.kind, BuildingKind::Tower | BuildingKind::HighTower))
            .map(|b| (b.x as i64 + b.w as i64 / 2, b.y as i64 + b.h as i64 / 2))
            .collect();
        // Ecktürme, die noch gehen (nicht am Hang), zählen, als stünden sie schon.
        centers.extend(
            layout
                .corner_towers()
                .into_iter()
                .filter(|&(x, y)| x >= 0 && y >= 0 && world.check_placement(p, BuildingKind::Tower, (x as u32, y as u32), 0, 1).is_ok())
                .map(|(x, y)| (x + 1, y + 1)),
        );
        let gates = layout.gates();
        let gate_rects: Vec<(i64, i64, i64, i64)> = world
            .buildings
            .iter()
            .filter(|b| b.owner == p && is_gate(b.kind))
            .map(|b| (b.x as i64, b.y as i64, (b.x + b.w) as i64 - 1, (b.y + b.h) as i64 - 1))
            .collect();
        let _ = gates;
        let own_wall = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && world.buildings.iter().any(|b| {
                    b.owner == p && matches!(b.kind, BuildingKind::Palisade | BuildingKind::Wall | BuildingKind::HighWall) && b.x as i64 == x && b.y as i64 == y
                })
        };
        for (x, y) in layout.ring_tiles() {
            if self.tower_skip.get(&(x, y)).is_some_and(|&t| self.clock < t) || !own_wall(x, y) {
                continue;
            }
            let far = centers.iter().all(|c| (c.0 - x).abs().max((c.1 - y).abs()) >= spacing);
            let off_gate = gate_rects.iter().all(|&(x0, y0, x1, y1)| {
                let dx = (x0 - x).max(x - x1).max(0);
                let dy = (y0 - y).max(y - y1).max(0);
                dx.max(dy) >= 3
            });
            if !far || !off_gate {
                continue;
            }
            let corner = ((x - 1).max(0) as u32, (y - 1).max(0) as u32);
            // Auf Probe mit dem jetzigen Lager; reicht es nicht, später noch einmal.
            match world.check_placement(p, BuildingKind::Tower, corner, 0, 1) {
                Ok(()) => return Some(corner),
                Err(e) if e.key == "error.not_enough" => return None,
                Err(_) => {
                    self.tower_skip.insert((x, y), self.clock + RETRY_SECONDS * 4.0);
                }
            }
        }
        None
    }

    /// Beschädigte Befestigungen (und den Bergfried) ausbessern, eine je Überlegung.
    fn repair(&self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        if !self.profile.repair {
            return;
        }
        let p = self.player;
        let damaged = world
            .buildings
            .iter()
            .filter(|b| b.owner == p)
            .filter(|b| world.economy.building(b.kind).is_some_and(|d| d.category == BuildingCategory::Fortification && b.hp < d.at(b.level).max_hp * 3 / 4))
            .min_by_key(|b| b.hp);
        if let Some(b) = damaged {
            if world.repair_cost(b).iter().all(|c| have(stock, c.good) >= c.amount) {
                cmds.push(ClientMsg::RepairBuilding { building_id: b.id });
            }
        }
    }

    /// Welche Waren fehlen schon lange? Dafür einen Betrieb mehr (`expand_seconds`).
    fn track_shortage(&mut self, world: &World) {
        let now = std::mem::take(&mut self.short_now);
        self.short_since.retain(|g, _| now.contains(g));
        for &g in &now {
            self.short_since.entry(g).or_insert(self.clock);
        }
        let Some(wait) = self.profile.expand_seconds else { return };
        let long: Vec<Good> = self.short_since.iter().filter(|&(_, &t)| self.clock - t >= wait).map(|(&g, _)| g).collect();
        for good in long {
            self.short_since.insert(good, self.clock);
            let Some(kind) = producer(world, good) else { continue };
            let have = self.count(world, kind);
            // Erst wenn er schon einen hat (sonst kommt der mit der Bauliste).
            if have == 0 {
                continue;
            }
            let planned = self.profile.build_order.iter().filter(|e| e.0 == kind).map(|e| e.1).max().unwrap_or(0);
            let want = self.extra.get(&kind).copied().unwrap_or(planned).max(have) + 1;
            if want <= planned.max(1) + self.profile.expand_max {
                debug!("Computer {}: {good:?} fehlt lange, baut noch ein {kind:?} ({want})", self.player);
                self.extra.insert(kind, want);
            }
        }
    }

    /// Wege: erst die Straßen vom Bergfried zu den Toren, dann (je nach Profil) vom Tor hinaus
    /// zu den Abbaubetrieben, so weit der Bauradius reicht. Läuft neben allem anderen her,
    /// sobald ein Sägewerk steht, und nur mit dem, was über der Holzreserve liegt.
    /// `wood_ok`: Holzweg als Ersatz für Pflaster erlaubt (nicht, solange die Mauer Holz braucht).
    fn build_roads(&mut self, world: &World, layout: &Layout, budget: &mut Budget, wood_ok: bool, cmds: &mut Vec<ClientMsg>) {
        let Some(preferred) = self.profile.road_kind else { return };
        // Erst die Burg zumachen: Wege brauchen dasselbe Holz und denselben Stein.
        if self.profile.walls && !self.closed {
            return;
        }
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
        // Viel Geld übrig: was zuletzt fehlte (Holz, Stein, Bretter ...), in kleinen Mengen zukaufen.
        let spare = trade.taler_reserve + SPARE_TALER;
        let mut short: Vec<Good> = self.short_since.keys().copied().collect();
        short.sort();
        for good in short {
            let Some(pr) = price(good) else { continue };
            if taler <= spare {
                break;
            }
            let n = ((taler - spare) / pr.buy.max(1)).min(SPARE_BATCH);
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
        // Alles, was ausbildet: Kasernen, Apotheken.
        let trains = |k: BuildingKind| world.economy.building(k).is_some_and(|d| d.levels.iter().any(|l| !l.trains.is_empty()));
        let barracks: Vec<_> = world.buildings.iter().filter(|b| b.owner == self.player && trains(b.kind)).collect();
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
            let target_gone = world.is_eliminated(c.target) || !world.units.iter().any(|u| u.owner == c.target && u.kind == UnitKind::King);
            // Angekommen: alle stehen (oder kämpfen), oder die meisten sind am Ziel.
            let near_goal = alive.iter().filter(|u| (u.pos.0 - c.goal.0).hypot(u.pos.1 - c.goal.1) < 12.0).count();
            let all_stopped = alive.iter().all(|u| !u.is_moving() || u.attack_order().is_some());
            if c.arrived.is_none() && (all_stopped || near_goal * 3 >= alive.len() * 2) {
                c.arrived = Some(self.clock);
                c.last_progress = self.clock;
            }
            if alive.iter().any(|u| u.attack_order().is_some()) {
                c.last_progress = self.clock;
            }
            let stalled = c.arrived.is_some() && self.clock - c.last_progress > a.siege_seconds;
            if alive.is_empty() || target_gone || stalled || power < c.start_power * a.retreat_below {
                debug!("Computer {p}: Welle gegen {} kehrt um", c.target);
                if let Some(home) = rally_point(world, p, layout) {
                    if !c.units.is_empty() {
                        cmds.push(ClientMsg::MoveUnits { unit_ids: c.units.clone(), x: home.0, y: home.1, fast: None, append: None });
                    }
                }
                self.campaign = None;
                self.next_attack = self.clock + a.cooldown_seconds;
                return;
            }
            // Belagern: wer angekommen ist (oder unterwegs auf Feinde trifft), greift an.
            let busy: Vec<&crate::world::Unit> = alive
                .iter()
                .copied()
                .filter(|u| c.arrived.is_some() || (u.pos.0 - c.goal.0).hypot(u.pos.1 - c.goal.1) < 12.0 || !u.is_moving())
                .collect();
            let (engines, troops): (Vec<&crate::world::Unit>, Vec<&crate::world::Unit>) = busy.iter().copied().partition(|u| u.kind.is_siege());
            cmds.extend(attack::siege(world, p, c.target, &troops));
            cmds.extend(attack::bombard(world, p, c.target, &engines));
            // Wer unterwegs hängen geblieben ist (weit vom Ziel, steht, ohne Angriff), läuft weiter.
            if c.arrived.is_none() {
                let ordered: std::collections::HashSet<u32> = cmds
                    .iter()
                    .filter_map(|m| match m {
                        ClientMsg::Attack { unit_ids, .. } => Some(unit_ids.clone()),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                let stuck: Vec<u32> = alive
                    .iter()
                    .filter(|u| !u.is_moving() && u.attack_order().is_none() && !ordered.contains(&u.id))
                    .filter(|u| (u.pos.0 - c.goal.0).hypot(u.pos.1 - c.goal.1) > 8.0)
                    .map(|u| u.id)
                    .collect();
                if !stuck.is_empty() {
                    cmds.push(ClientMsg::MoveUnits { unit_ids: stuck, x: c.goal.0, y: c.goal.1, fast: None, append: None });
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
            .filter(|u| u.owner == p && fighter(u.kind) && !u.is_moving() && !u.is_on_wall() && !self.on_relief(u.id))
            .collect();
        ready.sort_by(|x, y| {
            let d = |u: &crate::world::Unit| (u.pos.0 - home.0).hypot(u.pos.1 - home.1);
            d(x).total_cmp(&d(y))
        });
        let mut wave: Vec<&crate::world::Unit> = ready.into_iter().skip(a.home_guard as usize).collect();
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
        // Bediente, fahrbare Belagerungsgeräte ziehen mit (ihre Bedienung fährt mit).
        let crews = world.units.iter().filter_map(|u| u.crew_of()).fold(HashMap::<u32, u32>::new(), |mut m, d| {
            *m.entry(d).or_default() += 1;
            m
        });
        wave.extend(world.units.iter().filter(|u| {
            u.owner == p
                && u.kind == UnitKind::Catapult
                && world.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).is_some_and(|s| crews.get(&u.id).copied().unwrap_or(0) >= s.crew)
        }));
        let units: Vec<u32> = wave.iter().map(|u| u.id).collect();
        cmds.push(ClientMsg::MoveUnits { unit_ids: units.clone(), x: goal.0, y: goal.1, fast: None, append: None });
        self.campaign = Some(Campaign { target, units, start_power: power, goal, arrived: None, last_progress: self.clock });
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
        if world.economy.building(BuildingKind::Keep).is_none_or(|d| d.at(keep.level).garrison == 0) {
            return;
        }
        let c = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        cmds.push(ClientMsg::MoveUnits { unit_ids: vec![king.id], x: c.0, y: c.1, fast: None, append: None });
    }

    /// Feinde nahe am Bergfried: alle Soldaten am Boden, die nicht mit einer Welle unterwegs
    /// sind, gehen auf den, der dem Bergfried am nächsten ist. `true`, solange er sich wehrt
    /// (dann schickt er keine neue Welle los).
    fn defend(&self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) -> bool {
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
        // Steht der Feind draußen vor der geschlossenen Mauer, bleiben die Nahkämpfer
        // drinnen hinter dem Tor (die Mauer hält ihn auf); nur Fernkämpfer schießen.
        let outside = !layout.inside(threat.pos.0 as i64, threat.pos.1 as i64) && crate::world::castle_closed(world, p);
        let ranged = |k: UnitKind| world.economy.unit(k).is_some_and(|d| world.economy.is_ranged(d.range));
        let defenders: Vec<u32> = world
            .units
            .iter()
            .filter(|u| u.owner == p && fighter(u.kind) && !u.is_on_wall() && !self.on_campaign(u.id))
            .filter(|u| !outside || ranged(u.kind))
            // Wer schon ein Ziel hat, bleibt dabei.
            .filter(|u| u.attack_order().is_none())
            .map(|u| u.id)
            .collect();
        if !defenders.is_empty() {
            debug!("Computer {p}: verteidigt gegen Spieler {}", threat.owner);
            cmds.push(ClientMsg::Attack { unit_ids: defenders, target: protocol::AttackTarget::Unit { id: threat.id }, force: None, ignite: None });
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
                    cmds.push(ClientMsg::MoveUnits { unit_ids: r.units.clone(), x: home.0, y: home.1, fast: None, append: None });
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
                cmds.push(ClientMsg::MoveUnits { unit_ids: idle, x: target.0, y: target.1, fast: None, append: None });
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
            .filter(|u| u.owner == p && fighter(u.kind) && !u.is_on_wall() && !self.on_campaign(u.id))
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
        cmds.push(ClientMsg::MoveUnits { unit_ids: units.clone(), x: at.0, y: at.1, fast: None, append: None });
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
    fn rally(&mut self, world: &World, layout: &Layout, cmds: &mut Vec<ClientMsg>) {
        let Some(point) = rally_point(world, self.player, layout) else { return };
        let p = self.player;
        // Wer beim letzten Mal auf einen Posten sollte und jetzt untätig unten steht, kam nicht
        // hin: diesen Posten eine Weile nicht mehr vergeben.
        let clock = self.clock;
        for (id, post) in std::mem::take(&mut self.post_sent) {
            let stuck = world.units.iter().find(|u| u.id == id).is_some_and(|u| {
                !u.is_moving() && !u.is_on_wall() && (u.pos.0 - post.0 as f32).hypot(u.pos.1 - post.1 as f32) > 2.0
            });
            if stuck {
                self.post_skip.insert(post, clock + POST_RETRY_SECONDS);
            }
        }
        self.post_skip.retain(|_, until| *until > clock);
        let post_key = |q: (f32, f32)| (q.0.round() as i64, q.1.round() as i64);
        let ranged = |k: UnitKind| world.economy.unit(k).is_some_and(|d| d.range > 2.0);
        let mut posts: Vec<((f32, f32), usize)> = Vec::new();
        if self.profile.garrison_ranged {
            // Wer oben steht oder auf dem Weg hinauf ist, an dieser Stelle.
            let up: Vec<(f32, f32)> = world
                .units
                .iter()
                .filter(|u| u.owner == p && fighter(u.kind))
                .filter_map(|u| match u.destination() {
                    Some((t, true)) => Some(t),
                    _ if u.is_on_wall() => Some(u.pos),
                    _ => None,
                })
                .collect();
            let in_rect = |b: &protocol::BuildingState, q: &(f32, f32)| {
                q.0 >= b.x as f32 && q.1 >= b.y as f32 && q.0 < (b.x + b.w) as f32 && q.1 < (b.y + b.h) as f32
            };
            let mut keep_post = None;
            for b in world.buildings.iter().filter(|b| b.owner == p) {
                let room = world.economy.building(b.kind).map_or(0, |d| d.at(b.level).garrison as usize);
                if room == 0 {
                    continue;
                }
                let taken = up.iter().filter(|q| in_rect(b, q)).count();
                if room > taken {
                    let c = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
                    if b.kind == BuildingKind::Keep {
                        keep_post = Some((c, room - taken));
                    } else {
                        posts.push((c, room - taken));
                    }
                }
            }
            // Türme zuerst, dann der Wehrgang der Mauer, zuletzt der Bergfried.
            if self.profile.man_walls {
                posts.extend(self.wall_posts(world, layout, &up).into_iter().map(|q| (q, 1)));
            }
            posts.extend(keep_post);
        }
        let idle: Vec<&crate::world::Unit> = world
            .units
            .iter()
            .filter(|u| u.owner == p && fighter(u.kind) && !u.is_moving() && !u.is_on_wall() && !self.on_campaign(u.id) && !self.on_relief(u.id))
            .collect();
        let mut to_rally = Vec::new();
        for u in idle {
            if ranged(u.kind) {
                if let Some(post) = posts.iter_mut().find(|(q, room)| *room > 0 && !self.post_skip.contains_key(&post_key(*q))) {
                    post.1 -= 1;
                    self.post_sent.insert(u.id, post_key(post.0));
                    cmds.push(ClientMsg::MoveUnits { unit_ids: vec![u.id], x: post.0 .0, y: post.0 .1, fast: None, append: None });
                    continue;
                }
            }
            if (u.pos.0 - point.0).hypot(u.pos.1 - point.1) > RALLY_SLACK {
                to_rally.push(u.id);
            }
        }
        if !to_rally.is_empty() {
            cmds.push(ClientMsg::MoveUnits { unit_ids: to_rally, x: point.0, y: point.1, fast: None, append: None });
        }
    }
}

impl Computer {
    /// Freie Plätze auf dem Wehrgang (eigene Mauern und Tore mit Zinnen), jedes zweite
    /// Feld, nahe dem Haupttor zuerst. Nur wenn eine Treppe hinaufführt.
    fn wall_posts(&self, world: &World, layout: &Layout, up: &[(f32, f32)]) -> Vec<(f32, f32)> {
        let p = self.player;
        let def = |k: BuildingKind| world.economy.building(k);
        let stairs = world
            .buildings
            .iter()
            .any(|b| b.owner == p && def(b.kind).is_some_and(|d| d.at(b.level).stairs && b.kind != BuildingKind::Keep));
        if !stairs {
            return Vec::new();
        }
        let walkway = |b: &&protocol::BuildingState| {
            b.owner == p
                && matches!(b.kind, BuildingKind::Palisade | BuildingKind::Wall | BuildingKind::HighWall)
                && def(b.kind).is_some_and(|d| d.at(b.level).walkway)
        };
        let main = layout.gates().first().map_or(layout.mid, |g| (g.1 as i64, g.2 as i64));
        let mut tiles: Vec<(i64, i64)> = world.buildings.iter().filter(walkway).map(|b| (b.x as i64, b.y as i64)).collect();
        tiles.sort_by_key(|&(x, y)| ((x - main.0).abs() + (y - main.1).abs(), x, y));
        let mut chosen: Vec<(i64, i64)> = Vec::new();
        let mut out = Vec::new();
        for (x, y) in tiles {
            if chosen.iter().any(|c| (c.0 - x).abs().max((c.1 - y).abs()) < 2) {
                continue;
            }
            chosen.push((x, y));
            let q = (x as f32 + 0.5, y as f32 + 0.5);
            if !up.iter().any(|u| (u.0 - q.0).hypot(u.1 - q.1) < 1.0) {
                out.push(q);
            }
        }
        out
    }
}

/// Ergebnis von `build_next`.
enum Next {
    /// Hat das gebaut (oder für diesmal keinen Platz gefunden: `None`).
    Placed(Option<BuildingKind>),
    /// Wartet auf Material für dieses Gebäude: so viel fehlt noch.
    Waiting(BuildingKind, Vec<GoodAmount>),
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

    /// Was für die erste Stufe fehlt (über die Reserven hinaus).
    fn short(&self, world: &World, kind: BuildingKind) -> Vec<Good> {
        self.short_cost(&world.economy.building(kind).map(|d| level_cost(d, 1)).unwrap_or_default())
    }

    fn short_cost(&self, cost: &[GoodAmount]) -> Vec<Good> {
        cost.iter()
            .filter(|c| self.left.get(&c.good).copied().unwrap_or(0) < c.amount + self.keep(c.good))
            .map(|c| c.good)
            .collect()
    }

    fn keep(&self, good: Good) -> u32 {
        match good {
            Good::Wood => self.wood_reserve,
            Good::Stone => self.stone_reserve,
            _ => 0,
        }
    }

    /// Reicht es für diese Kosten (etwa einen Ausbau)? Dann gleich abziehen.
    fn take(&mut self, cost: &[GoodAmount]) -> bool {
        if !self.short_cost(cost).is_empty() {
            return false;
        }
        for c in cost {
            *self.left.entry(c.good).or_default() -= c.amount;
        }
        true
    }

    fn pay(&mut self, world: &World, kind: BuildingKind) {
        for c in world.economy.building(kind).map(|d| level_cost(d, 1)).unwrap_or_default() {
            let left = self.left.entry(c.good).or_default();
            *left = left.saturating_sub(c.amount);
        }
    }
}

/// Eigene Kämpfer für Wellen, Wache und Posten: Soldaten ohne die Baumeister (die bauen und bedienen Geräte).
fn fighter(kind: UnitKind) -> bool {
    kind.is_soldier() && kind != UnitKind::Builder
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
    world.economy.building(kind).is_some_and(|d| d.is_road())
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

/// Betrieb, der diese Ware herstellt (erste Stufe, in der sie geht; erster in der Datei).
fn producer(world: &World, good: Good) -> Option<BuildingKind> {
    world.economy.def.buildings.iter().find_map(|b| {
        let first = b.levels.first()?;
        first
            .recipes
            .iter()
            .any(|id| world.economy.recipe(id).is_some_and(|r| r.outputs.iter().any(|o| o.good == good)))
            .then_some(b.kind)
    })
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
