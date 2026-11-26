//! Spielzustand: Einheiten und Gebäude auf der Karte.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use protocol::{
    BuildingKind, BuildingSettings, BuildingState, BuildingStatus, GateMode, GoodAmount, PeopleState, ResourceKind, ServerMsg, StockState, Text,
    TileKind, UnitActivity, UnitInfo, UnitKind, UnitState,
};

use crate::{economy::Economy, map::GameMap, stock::Stock};

mod alerts;
mod army;
mod bridge;
pub use bridge::NEUTRAL;
mod diplomacy;
mod ballistics;
mod combat;
mod crowd;
mod errand;
mod enclosure;
pub use enclosure::castle_closed;
pub use gate::{is_gate, passage_tiles};
mod gate;
mod heal;
mod market;
mod gather;
mod horses;
mod save;
mod siege;
mod storage;
pub use save::SaveGame;
mod people;
mod production;
mod progress;
mod transport;
use production::Work;
use gather::Gather;
use transport::Job;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Unit {
    pub id: u32,
    pub owner: u32,
    pub kind: UnitKind,
    pub pos: (f32, f32),
    /// Noch abzulaufende Wegpunkte, der nächste zuerst.
    path: Vec<(f32, f32)>,
    /// Wohin die Einheit zuletzt geschickt wurde (für neue Wegsuche, wenn
    /// plötzlich ein Gebäude im Weg steht).
    target: Option<(f32, f32)>,
    /// Träger und Karren: das Gebäude, zu dem sie gehören.
    home: Option<u32>,
    /// Laufender Transportauftrag.
    job: Option<Job>,
    /// Ladung von Träger oder Karren.
    carrying: Option<GoodAmount>,
    /// Sekunden bis zur nächsten Auftragssuche.
    cooldown: f32,
    /// Arbeiter: was er gerade tut.
    gather: Option<Gather>,
    /// Steht oben auf dem Wehrgang.
    #[serde(default)]
    on_wall: bool,
    /// Ebene je Wegpunkt in `path` (`true` = oben). Leer = alles am Boden.
    #[serde(default)]
    layers: Vec<bool>,
    /// Liegt das Ziel oben auf dem Wehrgang?
    #[serde(default)]
    target_wall: bool,
    /// Handelskarren: was er wohin bringt.
    #[serde(default)]
    trade: Option<diplomacy::Delivery>,
    /// Erlittener Schaden (Lebenspunkte = `hp` der Einheitenart minus das).
    #[serde(default)]
    damage: u32,
    /// Sekunden bis zum nächsten Schlag oder Schuss.
    #[serde(default)]
    attack_cooldown: f32,
    /// Zähler der Schläge und Schüsse (für die Animation im Client).
    #[serde(default)]
    strikes: u32,
    /// Angriffsbefehl: dieses Ziel verfolgen und angreifen, bis es fällt.
    #[serde(default)]
    attack_order: Option<protocol::AttackTarget>,
    /// Angriff mit „Alles angreifen“: das Ziel darf auch eigen, verbündet oder neutral sein.
    #[serde(default)]
    attack_force: bool,
    /// Sekunden bis zur nächsten Wegsuche beim Verfolgen.
    #[serde(default)]
    chase_timer: f32,
    /// Sitzt auf einem Pferd.
    #[serde(default)]
    mounted: bool,
    /// Schaden am Pferd (Lebenspunkte des Pferdes = `army.horse_hp` minus das).
    #[serde(default)]
    horse_damage: u32,
    /// Nach dem Sturz vom Pferd: so viele Sekunden noch am Boden.
    #[serde(default)]
    down: f32,
    /// Unterwegs zu dieser Pferdezucht, um aufzusteigen.
    #[serde(default)]
    mount_at: Option<u32>,
    /// Bewohner: Sekunden bis zur eigenen nächsten Mahlzeit (`None` = noch nicht verteilt).
    #[serde(default)]
    meal_in: Option<f32>,
    /// Bewohner: bei der letzten Mahlzeit nichts bekommen, arbeitet und läuft langsamer.
    #[serde(default)]
    hungry: bool,
    /// Bewohner: bei der letzten Mahlzeit die volle (gewählte) Ration bekommen.
    #[serde(default)]
    well_fed: bool,
    /// Gruppenmarsch: höchstens so schnell (Felder pro Sekunde), damit die Gruppe zusammenbleibt.
    #[serde(default)]
    speed_cap: Option<f32>,
    /// Heiler: Sekunden bis zur nächsten Behandlung.
    #[serde(default)]
    heal_cooldown: f32,
    /// Weitere Ziele nach dem aktuellen (Strg+Rechtsklick), das nächste zuerst.
    #[serde(default)]
    waypoints: Vec<Waypoint>,
    /// Zuletzt von selbst angegriffene Einheit und wie lange das her ist (für die Anzeige).
    #[serde(skip)]
    last_foe: Option<(u32, f32)>,
    /// Wer diese Einheit zuletzt angegriffen hat und wie lange das her ist (Sekunden).
    #[serde(default)]
    attacked_by: Option<(u32, f32)>,
    /// Gegenwehr: dieser Angriffsbefehl (auf ein Gebäude) ruht, bis der Angreifer fällt
    /// oder die Zeit (Sekunden) um ist; dann geht es dort weiter.
    #[serde(default)]
    resume_order: Option<(protocol::AttackTarget, f32)>,
    /// Tempo im letzten Schritt (Felder pro Sekunde): Schützen halten danach vor.
    #[serde(skip)]
    vel: (f32, f32),
    /// Baumeister: bedient dieses Belagerungsgerät (Id).
    #[serde(default)]
    crew_of: Option<u32>,
    /// Balliste: steht auf diesem Turm (Gebäude-Id).
    #[serde(default)]
    perch: Option<u32>,
    /// Belagerungsgerät: seit dem letzten vollen Vorrat abgegebene Schüsse.
    #[serde(default)]
    shots_fired: u32,
    /// Belagerungsgerät: angelieferter Nachschub, der noch keinen ganzen Schuss ergibt.
    #[serde(default)]
    ammo_part: u32,
}

/// Ein späteres Ziel eines Laufbefehls (siehe `Unit::waypoints`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Waypoint {
    to: (f32, f32),
    wall: bool,
    /// Gruppentempo für dieses Stück.
    cap: Option<f32>,
}

impl Unit {
    /// Sitzt auf einem Pferd.
    pub fn mounted(&self) -> bool {
        self.mounted
    }

    pub(crate) fn new(id: u32, owner: u32, kind: UnitKind, pos: (f32, f32)) -> Unit {
        Unit {
            id,
            owner,
            kind,
            pos,
            path: Vec::new(),
            target: None,
            home: None,
            job: None,
            carrying: None,
            cooldown: 0.0,
            gather: None,
            on_wall: false,
            layers: Vec::new(),
            target_wall: false,
            trade: None,
            damage: 0,
            attack_cooldown: 0.0,
            strikes: 0,
            attack_order: None,
            attack_force: false,
            chase_timer: 0.0,
            mounted: false,
            horse_damage: 0,
            down: 0.0,
            mount_at: None,
            meal_in: None,
            hungry: false,
            well_fed: false,
            speed_cap: None,
            waypoints: Vec::new(),
            last_foe: None,
            attacked_by: None,
            resume_order: None,
            heal_cooldown: 0.0,
            vel: (0.0, 0.0),
            crew_of: None,
            perch: None,
            shots_fired: 0,
            ammo_part: 0,
        }
    }

    /// Steht die Einheit oben (Mauer, Turm, Bergfried)?
    pub fn is_on_wall(&self) -> bool {
        self.on_wall
    }

    /// Läuft die Einheit gerade (hat noch Wegpunkte)?
    pub fn is_moving(&self) -> bool {
        !self.path.is_empty()
    }

    /// Laufender Angriffsbefehl (für die Computergegner).
    pub fn attack_order(&self) -> Option<protocol::AttackTarget> {
        self.attack_order
    }

    /// Wohin sie zuletzt geschickt wurde und ob das oben auf dem Wehrgang liegt.
    pub fn destination(&self) -> Option<((f32, f32), bool)> {
        self.target.map(|t| (t, self.target_wall))
    }

    /// Träger und Karren gehorchen keinen Befehlen, sie arbeiten von selbst.
    fn is_transport(&self) -> bool {
        matches!(self.kind, UnitKind::Carrier | UnitKind::Cart)
    }

    /// Träger, Karren, Handelskarren und Arbeiter: gehorchen keinen Befehlen und weichen Gebäuden aus.
    fn is_civilian(&self) -> bool {
        self.is_transport() || matches!(self.kind, UnitKind::Worker | UnitKind::TradeCart)
    }
}

/// Abstand der Plätze in einer Formation (Felder).
const FORMATION_GAP: f32 = 0.6;

pub struct World {
    pub map: GameMap,
    pub units: Vec<Unit>,
    pub buildings: Vec<BuildingState>,
    /// Pro Feld die Id des Gebäudes darauf, 0 = frei.
    occupied: Vec<u32>,
    /// Felder vor Eingängen (Feld-Index → Gebäude-Id): dort darf nichts gebaut werden.
    entrances: HashMap<usize, u32>,
    next_unit_id: u32,
    next_building_id: u32,
    pub economy: Economy,
    /// Gemeinsamer Vorrat je Spieler (liegt im Bergfried und in den Vorratslagern).
    stocks: HashMap<u32, Stock>,
    /// Eigenes kleines Lager je Betrieb (Gebäude-Id): Eingänge und fertige Waren.
    inventories: HashMap<u32, Stock>,
    /// Gebäude, deren Status oder Lager sich geändert hat (gehen gesammelt raus).
    dirty_buildings: BTreeSet<u32>,
    /// Spieler, deren Lager sich seit dem letzten Senden geändert hat.
    dirty_stocks: BTreeSet<u32>,
    /// Wie viel jedes Feld noch hergibt (Wald, Stein, Eisen).
    resource_left: Vec<u32>,
    /// Laufende Arbeit je Gebäude-Id.
    work: HashMap<u32, Work>,
    /// Sekunden seit dem letzten geschickten Fortschritt je Gebäude (siehe `progress.rs`).
    progress_clock: HashMap<u32, f32>,
    /// Änderungen für alle Clients (Gebäudestatus, Rohstoffe), siehe `take_events`.
    events: Vec<ServerMsg>,
    /// Bewohner je Spieler (Wohnplätze, Arbeiter, Hunger).
    people: HashMap<u32, PeopleState>,
    /// Sekunden bis zur nächsten Mahlzeit irgendeines Bewohners, je Spieler.
    meal_soonest: HashMap<u32, f32>,
    /// Automatischer Handel je Spieler (siehe `market.rs`).
    auto_trade: HashMap<u32, Vec<protocol::AutoTrade>>,
    /// Sekunden, die jeder Betrieb schon auf Träger wartet (Vorrang beim Verteilen, siehe `transport.rs`).
    transport_wait: HashMap<u32, f32>,
    /// Grenzen je Ware für neue Betriebe, je Spieler (Ressourcen-Fenster).
    good_limits: HashMap<u32, Vec<protocol::GoodLimit>>,
    /// Gewählte Ration und Steuersatz je Spieler (Indizes; fehlt = Voreinstellung).
    policies: HashMap<u32, (u32, u32)>,
    /// Steuern, die noch keinen ganzen Taler ergeben (in Prozent eines Talers).
    tax_carry: HashMap<u32, u32>,
    /// Sekunden seit dem letzten automatischen Handel.
    auto_trade_clock: f32,
    /// Tempo-Faktor je Betrieb, dessen Arbeiter (teils) hungern (siehe `people.rs`).
    worker_hunger: HashMap<u32, f32>,
    /// Betriebe ohne Arbeiter (keine Wohnung frei).
    unstaffed: HashSet<u32>,
    /// Träger und Karren ohne Wohnung: sie warten zu Hause.
    idle_units: HashSet<u32>,
    /// Zustand der Abbaubetriebe, wie ihre Arbeiter ihn melden.
    gather_status: HashMap<u32, BuildingStatus>,
    /// Sekunden seit dem letzten Zahltag je Spieler.
    pay_timer: HashMap<u32, f32>,
    /// Spieler, deren Sold am letzten Zahltag nicht gereicht hat.
    unpaid: HashSet<u32>,
    /// Spieler, deren Soldaten am letzten Zahltag nicht satt wurden.
    army_hungry: HashSet<u32>,
    /// Neue Betriebe: trägt der Arbeiter selbst? Im Spiel immer ja; Tests für Träger schalten es ab.
    pub(crate) worker_carries_default: bool,
    /// Spieler (Computer), bei denen neue Betriebe auch neben dem Wagenschuppen selbst tragen.
    pub(crate) self_carry: HashSet<u32>,
    /// Rechenzeit der Teilschritte seit dem letzten Abholen (Serverauslastung).
    pub phase_us: crate::profile::PhaseTimes,
    /// Zufall für Treffer im Fernkampf (Deckung), siehe `combat.rs`.
    combat_rng: u64,
    /// Pfeile und Bolzen in der Luft: Schaden erst beim Aufschlag (siehe `combat.rs`).
    projectiles: Vec<combat::Projectile>,
    /// Steine und Bolzen der Belagerungsgeräte in der Luft (siehe `siege.rs`).
    siege_shots: Vec<siege::SiegeShot>,
    /// Sekunden seit der letzten Meldung des Baufortschritts.
    site_clock: f32,
    /// Was in welchem Lager liegt (Summe = Vorrat), siehe `storage.rs`.
    hub_goods: HashMap<u32, storage::HubGoods>,
    /// Wie Waren zwischen Verbündeten ankommen.
    trade_mode: protocol::TradeMode,
    /// Fernkampf: Wurfbahn oder Sichtlinie (Lobby), siehe `ballistics.rs`.
    shot_mode: protocol::ShotMode,
    /// Spieler, deren König gefallen ist (siehe `combat.rs`).
    eliminated: BTreeSet<u32>,
    /// Bündnis je Spieler; wer fehlt, kämpft allein (eigene Nummer). Siehe `diplomacy.rs`.
    teams: HashMap<u32, u32>,
    /// Gewinner, sobald das Spiel entschieden ist.
    winners: Option<Vec<u32>>,
    /// Gespielte Spielzeit in Sekunden (ohne Pausen, mit Spieltempo), bis das Spiel entschieden ist.
    pub(crate) game_seconds: f64,
    /// Sekunden, seit einem Gebäude ein Arbeiter, Träger oder Karren fehlt (siehe `people.rs`).
    respawn_clock: HashMap<u32, f32>,
    /// Warnungen an Spieler (siehe `alerts.rs`).
    alerts: alerts::Alerts,
    /// Aufgeschobene Weg-Neuberechnung: (Uhr der ersten, Uhr der letzten Änderung).
    replan_pending: Option<(f64, f64)>,
    /// Uhr dafür (Sekunden Spielzeit, läuft auch nach Spielende weiter).
    replan_clock: f64,
}

/// Was es kostet, gleich auf `level` zu bauen: alle Stufen bis dahin zusammen.
pub fn level_cost(def: &protocol::BuildingDef, level: u8) -> Vec<GoodAmount> {
    let mut sum: Vec<GoodAmount> = Vec::new();
    for c in def.levels.iter().take(level.max(1) as usize).flat_map(|l| &l.cost) {
        match sum.iter_mut().find(|s| s.good == c.good) {
            Some(s) => s.amount += c.amount,
            None => sum.push(*c),
        }
    }
    sum
}

/// Feld vor dem Eingang einer Grundfläche (x, y, w, h), mittig an der Vorderseite.
/// Kann außerhalb der Karte liegen (dann negativ oder zu groß).
fn entrance_tile(x: u32, y: u32, w: u32, h: u32, rotation: u8) -> (i64, i64) {
    let (x, y, w, h) = (x as i64, y as i64, w as i64, h as i64);
    match rotation % 4 {
        0 => (x + w / 2, y + h),
        1 => (x - 1, y + h / 2),
        2 => (x + (w - 1) / 2, y - 1),
        _ => (x + w, y + (h - 1) / 2),
    }
}

/// Fehlertext für den Client (Schlüssel, übersetzt wird dort).
type Refusal = Text;

/// Ergebnis der Prüfungen vor dem Bauen (siehe `World::placement_plan`).
struct PlacementPlan {
    rotation: u8,
    level: u8,
    w: u32,
    h: u32,
    cost: Vec<GoodAmount>,
    walls: Vec<u32>,
    entrance: Option<(u32, u32)>,
}

/// Name eines Rohstoffs wie im Protokoll (`forest`, ...), für Text-Platzhalter.
fn resource_key(r: ResourceKind) -> String {
    serde_json::to_value(r).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

impl World {
    #[cfg(test)]
    pub(crate) fn snapshot_unit_shots(&self, u: &Unit) -> Option<u32> {
        self.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).filter(|s| s.shots > 0).map(|s| s.shots.saturating_sub(u.shots_fired))
    }

    pub fn new(mut map: GameMap, economy: Economy) -> World {
        map.nav.set_slope_percent(economy.def.movement.slope_speed_percent);
        let n = (map.width * map.height) as usize;
        let resource_left = map.resources.iter().map(|&r| economy.resource_amount(r)).collect();
        let mut world = World {
            map,
            units: Vec::new(),
            buildings: Vec::new(),
            occupied: vec![0; n],
            entrances: HashMap::new(),
            next_unit_id: 1,
            next_building_id: 1,
            economy,
            stocks: HashMap::new(),
            inventories: HashMap::new(),
            dirty_buildings: BTreeSet::new(),
            dirty_stocks: BTreeSet::new(),
            resource_left,
            work: HashMap::new(),
            progress_clock: HashMap::new(),
            events: Vec::new(),
            people: HashMap::new(),
            meal_soonest: HashMap::new(),
            worker_hunger: HashMap::new(),
            auto_trade: HashMap::new(),
            good_limits: HashMap::new(),
            policies: HashMap::new(),
            tax_carry: HashMap::new(),
            transport_wait: HashMap::new(),
            auto_trade_clock: 0.0,
            unstaffed: HashSet::new(),
            idle_units: HashSet::new(),
            gather_status: HashMap::new(),
            pay_timer: HashMap::new(),
            unpaid: HashSet::new(),
            army_hungry: HashSet::new(),
            hub_goods: HashMap::new(),
            trade_mode: protocol::TradeMode::default(),
            shot_mode: protocol::ShotMode::default(),
            eliminated: BTreeSet::new(),
            teams: HashMap::new(),
            winners: None,
            game_seconds: 0.0,
            replan_pending: None,
            replan_clock: 0.0,
            respawn_clock: HashMap::new(),
            alerts: alerts::Alerts::default(),
            worker_carries_default: true,
            self_carry: HashSet::new(),
            phase_us: Default::default(),
            combat_rng: 0x9E37_79B9_7F4A_7C15,
            projectiles: Vec::new(),
            siege_shots: Vec::new(),
            site_clock: 0.0,
        };
        world.add_map_bridges();
        world
    }

    /// Änderungen seit dem letzten Aufruf, zum Verteilen an alle Clients.
    pub fn take_events(&mut self) -> Vec<ServerMsg> {
        std::mem::take(&mut self.events)
    }

    pub fn stock_state(&self, player: u32) -> StockState {
        let mut state = self.stocks.get(&player).map(|s| s.state()).unwrap_or(StockState { goods: Vec::new(), capacity: 0, pools: Vec::new(), people: Default::default(), taler: 0, army: Default::default(), auto_trade: Vec::new(), limits: Vec::new() });
        state.auto_trade = self.auto_trade.get(&player).cloned().unwrap_or_default();
        state.limits = self.good_limits.get(&player).cloned().unwrap_or_default();
        state.people = self.people.get(&player).cloned().unwrap_or_default();
        state.people.next_meal_in = self.meal_soonest.get(&player).copied().unwrap_or(0.0).max(0.0).ceil() as u32;
        state.army = self.army_state(player);
        // Pferde liegen in den Pferdezuchten, zählen für die Anzeige (und Ausbildung) aber zum Vorrat.
        let horses = self.horses_available(player);
        state.goods.retain(|g| g.good != protocol::Good::Horse);
        if horses > 0 {
            state.goods.push(GoodAmount { good: protocol::Good::Horse, amount: horses });
        }
        state
    }

    /// Lager, die sich geändert haben, seit dem letzten Aufruf.
    pub fn take_dirty_stocks(&mut self) -> Vec<(u32, StockState)> {
        let dirty = std::mem::take(&mut self.dirty_stocks);
        dirty.into_iter().filter(|p| self.stocks.contains_key(p)).map(|p| (p, self.stock_state(p))).collect()
    }

    /// Kapazität je Ware: Grundwert plus je Vorratslager.
    /// Lagerplatz je Ware = Summe der eigenen Lager (Bergfried `base_capacity`, Vorratslager
    /// `storehouse_capacity`). Ohne Lager gibt es keinen Platz; was schon da ist, bleibt.
    fn update_capacity(&mut self, player: u32) {
        let hubs: Vec<u32> = self
            .buildings
            .iter()
            .filter(|b| b.owner == player && self.economy.building(b.kind).is_some_and(|d| d.hub))
            .map(|b| b.id)
            .collect();
        // Jedes Lager hat einen Gesamtplatz für die Waren, die es nimmt.
        let mut pools: Vec<crate::stock::Pool> = Vec::new();
        for &h in &hubs {
            let capacity = self.hub_capacity(h);
            let b = self.buildings.iter().find(|b| b.id == h).expect("eben gefunden");
            let goods = self.hub_allowed(b);
            match pools.iter_mut().find(|p| p.kind == b.kind && p.goods == goods) {
                Some(p) => p.capacity += capacity,
                None => pools.push(crate::stock::Pool { kind: b.kind, goods, capacity }),
            }
        }
        if let Some(s) = self.stocks.get_mut(&player) {
            s.pooled = true;
            s.pools = pools;
            self.dirty_stocks.insert(player);
        }
    }

    /// Eigenes Lager eines Betriebs je Ware, getrennt nach Eingängen (was ein
    /// Rezept dieser Gebäudeart verbraucht) und allem anderen (Ergebnisse).
    /// 0 im Balancing = `transport.building_capacity`.
    fn inventory_limits(&self, kind: BuildingKind, level: u32) -> (u32, BTreeMap<protocol::Good, u32>) {
        let def = self.economy.building(kind).map(|d| d.at(level));
        let or_default = |n: u32| if n > 0 { n } else { self.economy.def.transport.building_capacity.max(1) };
        let outputs = or_default(def.map_or(0, |d| d.storage_outputs));
        let inputs = or_default(def.map_or(0, |d| d.storage_inputs));
        let limits = self
            .economy
            .def
            .recipes
            .iter()
            .filter(|r| r.building == kind)
            .flat_map(|r| r.inputs.iter().map(|i| (i.good, inputs)))
            .collect::<BTreeMap<_, _>>();
        // Pferdezucht: so viele Pferde passen hinein.
        let mut limits = limits;
        if let Some(n) = self.economy.building(kind).map(|d| d.horse_capacity).filter(|&n| n > 0) {
            limits.insert(protocol::Good::Horse, n);
        }
        (outputs, limits)
    }

    /// Lagergrößen aller Betriebe neu setzen (Balancing geändert, Spielstand geladen).
    pub(crate) fn update_inventory_capacities(&mut self) {
        let caps: Vec<_> = self.buildings.iter().map(|b| (b.id, self.inventory_limits(b.kind, b.level))).collect();
        for (id, (cap, limits)) in caps {
            if let Some(inv) = self.inventories.get_mut(&id) {
                inv.capacity = cap;
                inv.limits = limits;
            }
        }
    }

    /// Neue Wirtschaftsdaten aus dem Balancing-Editor übernehmen. Stehende
    /// Gebäude bleiben; Lagergrößen passen sich an, Rezepte, die es nicht mehr
    /// gibt, laufen nicht weiter.
    pub fn set_economy(&mut self, economy: Economy) {
        self.economy = economy;
        self.map.nav.set_slope_percent(self.economy.def.movement.slope_speed_percent);
        self.update_inventory_capacities();
        let players: Vec<u32> = self.stocks.keys().copied().collect();
        for p in players {
            self.update_capacity(p);
        }
        for b in &self.buildings {
            self.dirty_buildings.insert(b.id);
        }
    }

    /// Admin: Waren direkt ins Lager legen (höchstens bis zur Kapazität).
    pub fn admin_give(&mut self, player: u32, goods: &[GoodAmount], taler: u32) {
        let Some(stock) = self.stocks.get_mut(&player) else { return };
        stock.taler = stock.taler.saturating_add(taler);
        for g in goods {
            if self.economy.def.goods.contains(&g.good) {
                stock.add_unlimited(g.good, g.amount);
            }
        }
        self.dirty_stocks.insert(player);
    }

    /// Neuer Spieler: bekommt einen König auf einem freien Startfeld.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn spawn_player(&mut self, player: u32) {
        let taken: Vec<(u32, u32)> = self
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::King)
            .map(|u| (u.pos.0 as u32, u.pos.1 as u32))
            .collect();
        let spawns = &self.map.spawns;
        let (x, y) = spawns
            .iter()
            .copied()
            .find(|s| !taken.contains(s))
            .unwrap_or(spawns[player as usize % spawns.len()]);
        self.spawn_player_at(player, (x, y));
    }

    /// Neuer Spieler auf einem bestimmten Startfeld (aus der Lobby).
    pub fn spawn_player_at(&mut self, player: u32, (x, y): (u32, u32)) {
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        // Noch kein Lager, also kein Platz: die Startgüter liegen „virtuell“ im Vorrat und
        // kommen ins erste Lager, sobald es steht.
        let mut stock = Stock::pooled();
        for g in &self.economy.def.start_stock {
            stock.add_unlimited(g.good, g.amount);
        }
        stock.taler = self.economy.def.market.start_taler;
        self.stocks.insert(player, stock);
        // Startgebäude (Bergfried) an der Fahne, der König steht vor seinem Eingang.
        let mut king = (x as f32 + 0.5, y as f32 + 0.5);
        if let Some(kind) = self.economy.def.start_building {
            if let Some(b) = self.place_start_building(player, kind, (x, y)) {
                if let Some((ex, ey)) = b.entrance {
                    king = (ex as f32 + 0.5, ey as f32 + 0.5);
                }
            }
        }
        self.units.push(Unit::new(id, player, UnitKind::King, king));
    }

    /// Startgebäude möglichst mittig auf die Fahne setzen; passt es dort nicht
    /// (Hang, Wald, Wasser), den nächsten passenden Platz in der Nähe suchen.
    fn place_start_building(&mut self, player: u32, kind: BuildingKind, (sx, sy): (u32, u32)) -> Option<BuildingState> {
        let def = self.economy.building(kind)?.clone();
        let mut placed = None;
        let mut last_refusal = None;
        'search: for ring in 0..=12i64 {
            for dy in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs().max(dy.abs()) != ring {
                        continue;
                    }
                    for rotation in [0u8, 1, 2, 3] {
                        let (w, h) = if rotation % 2 == 1 { (def.h, def.w) } else { (def.w, def.h) };
                        let x = sx as i64 + dx - w as i64 / 2;
                        let y = sy as i64 + dy - h as i64 / 2;
                        if x < 0 || y < 0 {
                            continue;
                        }
                        let res = self.place_building_at(player, kind, (x as u32, y as u32), rotation, 1, true);
                        if let Err(e) = &res {
                            last_refusal = Some(e.clone());
                        }
                        if let Ok(b) = res {
                            // Mitspieler, die schon da sind, sollen ihn auch sehen.
                            self.events.push(ServerMsg::BuildingPlaced { building: b.clone() });
                            placed = Some(b);
                            break 'search;
                        }
                    }
                }
            }
        }
        if placed.is_none() {
            tracing::warn!("Startgebäude {kind:?} für Spieler {player} passt nicht in die Nähe der Fahne ({sx}, {sy}): {last_refusal:?}");
        }
        placed
    }

    /// Spieler geht: seine Einheiten und Gebäude verschwinden.
    /// Liefert die Ids der entfernten Gebäude.
    pub fn remove_player(&mut self, player: u32) -> Vec<u32> {
        self.units.retain(|u| u.owner != player);
        let ids: Vec<u32> = self.buildings.iter().filter(|b| b.owner == player).map(|b| b.id).collect();
        for &id in &ids {
            self.remove_building(id);
        }
        self.stocks.remove(&player);
        self.people.remove(&player);
        self.meal_soonest.remove(&player);
        self.auto_trade.remove(&player);
        self.good_limits.remove(&player);
        self.policies.remove(&player);
        self.tax_carry.remove(&player);
        ids
    }

    /// Gebäude errichten, wenn der Platz passt und das Lager reicht. Ein
    /// Gebäude braucht flachen Boden auf einer einzigen Höhenstufe, keine
    /// Rampe, kein Wasser, keinen Wald oder Fels, und darf weder ein anderes
    /// Gebäude noch eine Einheit überdecken.
    #[cfg(test)]
    /// Tests: einen Kornspeicher irgendwo frei hinstellen (von unten rechts gesucht), damit Essen
    /// nicht verdirbt (`spoil_unstored_food`). Bricht ab, wenn nirgends Platz ist.
    #[cfg(test)]
    pub(crate) fn test_granary(&mut self, player: u32) -> u32 {
        for y in (0..self.map.height).rev() {
            for x in (0..self.map.width).rev() {
                if let Ok(b) = self.place_building(player, BuildingKind::Granary, x, y) {
                    return b.id;
                }
            }
        }
        panic!("kein Platz für einen Kornspeicher");
    }

    pub fn place_building(&mut self, player: u32, kind: BuildingKind, x: u32, y: u32) -> Result<BuildingState, Refusal> {
        self.place_building_rotated(player, kind, x, y, 0, 1)
    }

    /// Wie `place_building`, gedreht um `rotation` Vierteldrehungen (siehe Protokoll).
    /// Vor dem Eingang muss ein begehbares Feld frei sein, und kein Gebäude darf
    /// den Eingang eines anderen zubauen.
    pub fn place_building_rotated(
        &mut self,
        player: u32,
        kind: BuildingKind,
        x: u32,
        y: u32,
        rotation: u8,
        level: u8,
    ) -> Result<BuildingState, Refusal> {
        if let Some(gate) = self.road_through_gate(player, kind, (x, y))? {
            return Ok(gate);
        }
        self.place_building_at(player, kind, (x, y), rotation, level, false)
    }

    /// Weg durch den Durchgang eines eigenen Tors ziehen: es entsteht kein eigenes
    /// Wegstück, der Durchgang wird so schnell wie der Weg. Liefert das Tor, oder
    /// None, wenn hier kein Tordurchgang liegt.
    fn road_through_gate(&mut self, player: u32, kind: BuildingKind, (x, y): (u32, u32)) -> Result<Option<BuildingState>, Refusal> {
        let Some(def) = self.economy.building(kind).cloned() else { return Ok(None) };
        let speed = def.at(1).road_speed_percent;
        if !def.is_road() || x >= self.map.width || y >= self.map.height {
            return Ok(None);
        }
        let id = self.occupied[(y * self.map.width + x) as usize];
        let Some(bi) = self.buildings.iter().position(|b| b.id == id) else { return Ok(None) };
        let b = &self.buildings[bi];
        if b.owner != player || !gate::is_gate(b.kind) || !gate::passage_tiles(b).contains(&(x, y)) {
            return Ok(None);
        }
        if b.road_under >= speed {
            return Err(Text::new("error.occupied"));
        }
        self.pay(player, &def.levels[0].cost)?;
        let old = self.buildings[bi].clone();
        self.set_footprint(&old, false);
        self.buildings[bi].road_under = speed;
        let b = self.buildings[bi].clone();
        self.set_footprint(&b, true);
        self.dirty_buildings.insert(b.id);
        self.replan_all();
        Ok(Some(b))
    }

    /// Prüft, ob der Bau so ginge (Platz, Regeln, Kosten), ohne etwas zu ändern.
    /// Dieselben Regeln wie `place_building_rotated`; der Computergegner sucht damit Bauplätze.
    pub fn check_placement(
        &self,
        player: u32,
        kind: BuildingKind,
        (x, y): (u32, u32),
        rotation: u8,
        level: u8,
    ) -> Result<(), Refusal> {
        let plan = self.placement_plan(player, kind, (x, y), rotation, level, false)?;
        let stock = self.stocks.get(&player).ok_or_else(|| Text::new("error.not_enough"))?;
        if !stock.has_all(&plan.cost) {
            return Err(Text::new("error.not_enough"));
        }
        Ok(())
    }

    /// Darf der Spieler noch eins davon bauen (`max_per_player`)? Hängt nicht vom Ort ab,
    /// die Bauplatzsuche der KI fragt das deshalb einmal vorab.
    pub fn check_limit(&self, player: u32, kind: BuildingKind) -> Result<(), Refusal> {
        let Some(max) = self.economy.building(kind).and_then(|d| d.max_per_player) else { return Ok(()) };
        let have = self.buildings.iter().filter(|b| b.owner == player && b.kind == kind).count();
        if have >= max as usize {
            return Err(Text::new("error.limit_reached").arg("max", max));
        }
        Ok(())
    }

    /// Alle Prüfungen vor dem Bauen außer dem Bezahlen.
    fn placement_plan(
        &self,
        player: u32,
        kind: BuildingKind,
        (x, y): (u32, u32),
        rotation: u8,
        level: u8,
        free: bool,
    ) -> Result<PlacementPlan, Refusal> {
        // Nicht klonen: die KI prüft bei der Bauplatzsuche zehntausende Stellen.
        let def = self.economy.building(kind).ok_or_else(|| Text::new("error.unknown_building"))?;
        // Baustellen für Belagerungsgeräte setzen nur Baumeister (`build_siege`).
        if kind == BuildingKind::SiegeSite {
            return Err(Text::new("error.unknown_building"));
        }
        let rotation = rotation % 4;
        let level = level.max(1);
        if level as usize > def.levels.len() {
            return Err(Text::new("error.max_level"));
        }
        let (w, h) = if rotation % 2 == 1 { (def.h, def.w) } else { (def.w, def.h) };
        // `first_free`: das erste Gebäude dieser Art ist gratis.
        let cost = if free || (def.first_free && !self.owns(player, kind)) { Vec::new() } else { level_cost(def, level) };
        self.check_limit(player, kind)?;
        // Das Startgebäude (der Bergfried) steht noch vor jedem Bergfried: kein Bauradius.
        if !free {
            self.check_build_radius(player, def.build_radius, (x, y), (w, h))?;
        }
        if !free {
            self.check_no_enemy(player, kind, (x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0))?;
        }
        // Tore und Türme dürfen auf eigene Mauern, alle Gebäude auf eigene Wege;
        // die kommen beim Bauen weg.
        let walls = self.removable_under(player, def, (x, y), (w, h));
        let entrance = self.check_site(def, (x, y), (w, h), rotation, &walls)?;
        let covers = |p: (f32, f32)| {
            let (ux, uy) = (p.0.floor() as i64, p.1.floor() as i64);
            ux >= x as i64 && uy >= y as i64 && ux < (x + w) as i64 && uy < (y + h) as i64
        };
        // Träger weichen aus, alle anderen Einheiten stehen im Weg (nicht bei Wegen).
        if !def.is_road() && self.units.iter().any(|u| !u.is_civilian() && covers(u.pos)) {
            return Err(Text::new("error.unit_in_way"));
        }
        Ok(PlacementPlan { rotation, level, w, h, cost, walls, entrance })
    }

    /// Wie `place_building_rotated`; `free`: kostet nichts (Startgebäude).
    fn place_building_at(
        &mut self,
        player: u32,
        kind: BuildingKind,
        (x, y): (u32, u32),
        rotation: u8,
        level: u8,
        free: bool,
    ) -> Result<BuildingState, Refusal> {
        let PlacementPlan { rotation, level, w, h, cost, walls, entrance } =
            self.placement_plan(player, kind, (x, y), rotation, level, free)?;
        let def = self.economy.building(kind).ok_or_else(|| Text::new("error.unknown_building"))?.clone();
        let max_hp = def.at(level as u32).max_hp;
        self.pay(player, &cost)?;
        // Tor auf einem Weg: der Durchgang bleibt so schnell wie der Weg.
        let road_under = if gate::is_gate(kind) {
            walls
                .iter()
                .filter_map(|id| self.buildings.iter().find(|b| b.id == *id))
                .filter_map(|b| self.economy.building(b.kind).map(|d| d.at(b.level).road_speed_percent))
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        for id in walls {
            if self.demolish(player, id).is_ok() {
                self.events.push(ServerMsg::BuildingRemoved { building_id: id });
            }
        }

        // Grenzen aus dem Ressourcen-Fenster für die Waren, die der Betrieb nutzt.
        let (keep_in_stock, stop_at) = self.default_limits(player, &def, level as u32);
        let settings = BuildingSettings {
            recipe: def.levels[0].recipes.first().cloned(),
            paused: false,
            accept_deliveries: def.delivery.carriers,
            stores: None,
            keep_in_stock,
            gate: GateMode::Auto,
            worker_carries: def.delivery.worker && self.worker_carries_default && (self.self_carry.contains(&player) || !self.cart_shed_covers(player, x, y, w, h)),
            cart_deliveries: def.delivery.carts,
            stop_at,
        };
        let status = if settings.recipe.is_some() { BuildingStatus::Working } else { BuildingStatus::Idle };
        let mut b = BuildingState {
            id: self.next_building_id,
            owner: player,
            kind,
            x,
            y,
            w,
            h,
            hp: max_hp,
            level: level as u32,
            settings,
            status,
            rotation,
            entrance,
            inventory: Vec::new(),
            gate_open: gate::is_gate(kind),
            training: Vec::new(),
            training_left: 0.0,
            progress: 0.0,
            progress_rate: 0.0,
            progress_seq: 0,
            road_under,
            capacity: 0,
            mature_in: def.release_after_seconds,
            rally: None,
            site: None,
        };
        self.next_building_id += 1;
        if let Some((ex, ey)) = entrance {
            self.entrances.insert((ey * self.map.width + ex) as usize, b.id);
        }
        self.set_footprint(&b, true);
        self.buildings.push(b.clone());
        if def.hub {
            let cap = self.hub_capacity(b.id);
            b.capacity = cap;
            self.buildings.last_mut().expect("eben angelegt").capacity = cap;
        }
        if !def.hub {
            let (cap, limits) = self.inventory_limits(b.kind, b.level);
            let mut inv = Stock::new(cap);
            inv.limits = limits;
            self.inventories.insert(b.id, inv);
        }
        if def.hub {
            self.update_capacity(player);
        }
        if !def.is_road() {
            self.make_room(&b);
        }
        let spawns: Vec<_> = def.levels.iter().take(level as usize).flat_map(|l| &l.spawns).copied().collect();
        for s in spawns {
            for n in 0..s.count {
                self.spawn_transport(&b, s.kind, n);
            }
        }
        self.spawn_workers(self.buildings.len() - 1);
        self.replan_all();
        Ok(b)
    }

    /// Darf ein Gebäude dieser Art (schon gedreht: `w` x `h`) hier stehen?
    /// `ignore`: dieses Gebäude zählt nicht als Hindernis (beim Versetzen).
    /// Liefert das Feld vor dem Eingang.
    fn owns(&self, player: u32, kind: BuildingKind) -> bool {
        self.buildings.iter().any(|b| b.owner == player && b.kind == kind)
    }

    /// Eigene Gebäude in dieser Fläche, die ein neues Gebäude dieser Art ersetzt:
    /// Mauerstücke (Mauer, hohe Mauer, Palisade), wenn es auf Mauern darf, und Wege.
    fn removable_under(&self, player: u32, def: &protocol::BuildingDef, (x, y): (u32, u32), (w, h): (u32, u32)) -> Vec<u32> {
        let replaces_roads = !def.is_road();
        let mut ids: Vec<u32> = Vec::new();
        for ty in y..(y + h).min(self.map.height) {
            for tx in x..(x + w).min(self.map.width) {
                let id = self.occupied[(ty * self.map.width + tx) as usize];
                if id == 0 || ids.contains(&id) {
                    continue;
                }
                let wall = self.buildings.iter().find(|b| b.id == id).is_some_and(|b| {
                    let wall = matches!(b.kind, BuildingKind::Wall | BuildingKind::HighWall | BuildingKind::Palisade);
                    let road = self.economy.building(b.kind).is_some_and(|d| d.is_road());
                    b.owner == player && ((def.on_walls && wall) || (replaces_roads && road))
                });
                if wall {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Liegt eines der Rohstoff-Felder höchstens `r` Felder vom Rand der Fläche entfernt
    /// (gleiche Abstandsregel wie beim Abbau)?
    fn resource_near(&self, res: &[ResourceKind], (x, y): (u32, u32), (w, h): (u32, u32), r: u32) -> bool {
        let r = r.min(64) as i64;
        let (x0, y0, x1, y1) = (x as i64, y as i64, (x + w) as i64 - 1, (y + h) as i64 - 1);
        let (mw, mh) = (self.map.width as i64, self.map.height as i64);
        ((y0 - r).max(0)..=(y1 + r).min(mh - 1)).any(|ty| {
            ((x0 - r).max(0)..=(x1 + r).min(mw - 1)).any(|tx| {
                let dx = (x0 - tx).max(tx - x1).max(0);
                let dy = (y0 - ty).max(ty - y1).max(0);
                dx * dx + dy * dy <= r * r && res.contains(&self.map.resources[(ty * mw + tx) as usize])
            })
        })
    }

    /// Grundhöhe einer Fläche: die Stufe, die die meisten Felder haben (bei Gleichstand die
    /// niedrigere). Schräge Felder zählen nur, wenn es keine ebenen gibt.
    pub fn base_level(&self, (x, y): (u32, u32), (w, h): (u32, u32)) -> u8 {
        let mut count: std::collections::BTreeMap<u8, (u32, u32)> = Default::default();
        for ty in y..(y + h).min(self.map.height) {
            for tx in x..(x + w).min(self.map.width) {
                let i = (ty * self.map.width + tx) as usize;
                let e = count.entry(self.map.levels[i]).or_default();
                if self.map.tiles[i] == TileKind::Ramp {
                    e.1 += 1;
                } else {
                    e.0 += 1;
                }
            }
        }
        let mut best: Option<(u8, (u32, u32))> = None;
        for (&level, &c) in &count {
            if best.is_none_or(|(_, b)| c > b) {
                best = Some((level, c));
            }
        }
        best.map_or(0, |(l, _)| l)
    }

    fn check_site(
        &self,
        def: &protocol::BuildingDef,
        (x, y): (u32, u32),
        (w, h): (u32, u32),
        rotation: u8,
        ignore: &[u32],
    ) -> Result<Option<(u32, u32)>, Refusal> {
        if x.checked_add(w).is_none_or(|r| r > self.map.width) || y.checked_add(h).is_none_or(|b| b > self.map.height) {
            return Err(Text::new("error.outside_map"));
        }
        let height = self.base_level((x, y), (w, h));
        // Ohne `on_ramps`: höchstens `slope_tiles` Felder dürfen schräg oder eine Stufe daneben liegen.
        let mut uneven = 0;
        for ty in y..y + h {
            for tx in x..x + w {
                let i = (ty * self.map.width + tx) as usize;
                if self.map.tiles[i] == TileKind::Water {
                    return Err(Text::new("error.on_water"));
                }
                let diff = self.map.levels[i].abs_diff(height);
                if diff > 1 {
                    return Err(Text::new("error.not_flat"));
                }
                if !def.on_ramps && (diff == 1 || self.map.tiles[i] == TileKind::Ramp) {
                    uneven += 1;
                    if uneven > self.economy.def.slope_tiles {
                        let ramp = self.map.tiles[i] == TileKind::Ramp;
                        return Err(Text::new(if ramp { "error.on_ramp" } else { "error.not_flat" }));
                    }
                }
                if self.occupied[i] != 0 && !ignore.contains(&self.occupied[i]) {
                    return Err(Text::new("error.occupied"));
                }
                // Wege dürfen vor Türen liegen, sie bleiben ja begehbar.
                if !def.is_road() && self.entrances.get(&i).is_some_and(|b| !ignore.contains(b)) {
                    return Err(Text::new("error.blocks_entrance"));
                }
                let r = self.map.resources[i];
                let allowed = def.placement.as_ref().is_some_and(|p| p.resources.contains(&r));
                if crate::map::resource_blocks(r) && !allowed {
                    return Err(Text::new("error.on_resource"));
                }
            }
        }
        if let Some(p) = &def.placement {
            let on = (y..y + h)
                .flat_map(|ty| (x..x + w).map(move |tx| (tx, ty)))
                .filter(|&(tx, ty)| p.resources.contains(&self.map.resources[(ty * self.map.width + tx) as usize]))
                .count() as u32;
            // Abbaubetriebe mit Reichweite (Steinbruch, Mine): Rohstoff im Umkreis genügt.
            let r = def.at(1).gather_radius;
            let near = !def.gathers.is_empty() && r > 0 && self.resource_near(&p.resources, (x, y), (w, h), r);
            if !near && on * 100 < p.min_share_percent * w * h {
                let names: Vec<String> = p.resources.iter().map(|r| resource_key(*r)).collect();
                return Err(Text::new("error.needs_resource").arg("resources", names.join(",")));
            }
        }
        let entrance = if def.no_entrance { None } else { Some(entrance_tile(x, y, w, h, rotation)) };
        if let Some((ex, ey)) = entrance {
            let inside = ex >= 0 && ey >= 0 && (ex as u32) < self.map.width && (ey as u32) < self.map.height;
            if !inside || !self.map.nav.is_open(ex as i32, ey as i32) {
                return Err(Text::new("error.entrance_blocked"));
            }
        }
        Ok(entrance.map(|(ex, ey)| (ex as u32, ey as u32)))
    }

    /// Was das Versetzen kostet: `move_cost_percent` aller Bau- und Ausbaukosten, aufgerundet.
    pub fn move_cost(&self, b: &BuildingState) -> Vec<GoodAmount> {
        let pct = self.economy.def.move_cost_percent;
        let Some(def) = self.economy.building(b.kind) else { return Vec::new() };
        level_cost(def, b.level as u8)
            .into_iter()
            .map(|g| GoodAmount { good: g.good, amount: (g.amount * pct).div_ceil(100) })
            .filter(|g| g.amount > 0)
            .collect()
    }

    /// Eigenes Gebäude an eine andere Stelle versetzen. Stufe, Einstellungen und
    /// Inhalt bleiben, seine Leute laufen zur neuen Stelle.
    pub fn move_building(&mut self, player: u32, id: u32, x: u32, y: u32, rotation: u8) -> Result<BuildingState, Refusal> {
        let bi = self.buildings.iter().position(|b| b.id == id).ok_or_else(|| Text::new("error.no_building"))?;
        let old = self.buildings[bi].clone();
        if old.kind == BuildingKind::Bridge {
            return Err(Text::new("error.bridge_fixed"));
        }
        if old.owner != player {
            return Err(Text::new("error.not_yours"));
        }
        let def = self.economy.building(old.kind).ok_or_else(|| Text::new("error.unknown_building"))?.clone();
        let rotation = rotation % 4;
        let (w, h) = if rotation % 2 == 1 { (def.h, def.w) } else { (def.w, def.h) };
        if self.units.iter().any(|u| u.on_wall && {
            let (ux, uy) = (u.pos.0.floor() as i64, u.pos.1.floor() as i64);
            ux >= old.x as i64 && uy >= old.y as i64 && ux < (old.x + old.w) as i64 && uy < (old.y + old.h) as i64
        }) {
            return Err(Text::new("error.unit_in_way"));
        }
        // Alte Fläche kurz freigeben, damit man auch um ein paar Felder versetzen kann.
        self.set_footprint(&old, false);
        let site = self.check_site(&def, (x, y), (w, h), rotation, &[id]).and_then(|entrance| {
            let covers = |p: (f32, f32)| {
                let (ux, uy) = (p.0.floor() as i64, p.1.floor() as i64);
                ux >= x as i64 && uy >= y as i64 && ux < (x + w) as i64 && uy < (y + h) as i64
            };
            if self.units.iter().any(|u| !u.is_civilian() && covers(u.pos)) {
                return Err(Text::new("error.unit_in_way"));
            }
            Ok(entrance)
        });
        let cost = self.move_cost(&old);
        let entrance = match site.and_then(|e| self.pay(player, &cost).map(|_| e)) {
            Ok(e) => e,
            Err(e) => {
                self.set_footprint(&old, true);
                return Err(e);
            }
        };
        self.entrances.retain(|_, b| *b != id);
        let b = &mut self.buildings[bi];
        (b.x, b.y, b.w, b.h, b.rotation, b.entrance) = (x, y, w, h, rotation, entrance);
        let b = b.clone();
        if let Some((ex, ey)) = entrance {
            self.entrances.insert((ey * self.map.width + ex) as usize, id);
        }
        self.set_footprint(&b, true);
        self.make_room(&b);
        self.drop_from_walls();
        self.replan_all();
        Ok(b)
    }

    /// Felder eines Gebäudes belegen (`true`) oder freigeben. Wege blockieren
    /// nicht, sondern machen das Feld schneller.
    pub(super) fn set_footprint(&mut self, b: &BuildingState, on: bool) {
        if b.kind == BuildingKind::Bridge {
            self.set_bridge_footprint(b, on);
            return;
        }
        let road = self.economy.building(b.kind).filter(|d| d.is_road()).map_or(0, |d| d.at(b.level).road_speed_percent.max(1));
        for ty in b.y..b.y + b.h {
            for tx in b.x..b.x + b.w {
                self.occupied[(ty * self.map.width + tx) as usize] = if on { b.id } else { 0 };
                if road > 0 {
                    let pct = if on { (100 + road).min(u16::MAX as u32) as u16 } else { 100 };
                    self.map.nav.set_speed(tx as i32, ty as i32, pct);
                } else if gate::blocks(b, tx, ty) {
                    self.map.nav.set_blocked(tx as i32, ty as i32, on);
                }
            }
        }
        if gate::is_gate(b.kind) && b.road_under > 0 {
            let pct = if on { (100 + b.road_under).min(u16::MAX as u32) as u16 } else { 100 };
            for (tx, ty) in gate::passage_tiles(b) {
                self.map.nav.set_speed(tx as i32, ty as i32, pct);
            }
        }
    }

    /// Kosten aus dem Lager nehmen oder mit Grund ablehnen.
    fn pay(&mut self, player: u32, cost: &[GoodAmount]) -> Result<(), Refusal> {
        let stock = self.stocks.get_mut(&player).ok_or_else(|| Text::new("error.no_player"))?;
        if let Some(missing) = cost.iter().find(|c| stock.amount(c.good) < c.amount) {
            let good = serde_json::to_value(missing.good).ok().and_then(|v| v.as_str().map(String::from));
            return Err(Text::new("error.not_enough")
                .arg("good", good.unwrap_or_default())
                .arg("need", missing.amount - stock.amount(missing.good)));
        }
        stock.take_all(cost);
        self.dirty_stocks.insert(player);
        Ok(())
    }

    /// Eigenes Gebäude auf die nächste Stufe bringen.
    pub fn upgrade(&mut self, player: u32, id: u32) -> Result<BuildingState, Refusal> {
        let b = self.buildings.iter().find(|b| b.id == id).ok_or_else(|| Text::new("error.no_building"))?;
        if b.owner != player {
            return Err(Text::new("error.not_yours"));
        }
        let def = self.economy.building(b.kind).ok_or_else(|| Text::new("error.unknown_building"))?;
        let Some(next) = def.levels.get(b.level as usize) else {
            return Err(Text::new("error.max_level"));
        };
        let cost = next.cost.clone();
        let spawns = next.spawns.clone();
        let (old_hp, new_hp, radius, road) = (def.at(b.level).max_hp, next.max_hp, def.build_radius, def.is_road());
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let old = b.clone();
        self.check_build_radius(player, radius, (old.x, old.y), (old.w, old.h))?;
        self.check_no_enemy(player, old.kind, center)?;
        self.pay(player, &cost)?;
        if road {
            self.set_footprint(&old, false);
        }
        let b = self.buildings.iter_mut().find(|b| b.id == id).expect("eben gefunden");
        b.level += 1;
        // Mehr (oder weniger) Lebenspunkte auf der neuen Stufe: der Schaden bleibt.
        b.hp = (b.hp + new_hp).saturating_sub(old_hp).clamp(1, new_hp.max(1));
        let b = b.clone();
        if road {
            self.set_footprint(&b, true);
        }
        let (cap, limits) = self.inventory_limits(b.kind, b.level);
        if let Some(inv) = self.inventories.get_mut(&b.id) {
            inv.capacity = cap;
            inv.limits = limits;
        }
        self.dirty_buildings.insert(b.id);
        for s in spawns {
            for n in 0..s.count {
                self.spawn_transport(&b, s.kind, n);
            }
        }
        if self.economy.building(b.kind).is_some_and(|d| d.hub) {
            self.update_capacity(player);
        }
        Ok(b)
    }

    /// Eigenes Gebäude abreißen. Ein Teil aller Bau- und Ausbaukosten kommt zurück.
    pub fn demolish(&mut self, player: u32, id: u32) -> Result<(), Refusal> {
        let b = match self.buildings.iter().find(|b| b.id == id) {
            Some(b) if b.owner == player => b.clone(),
            Some(_) => return Err(Text::new("error.not_yours")),
            None => return Err(Text::new("error.no_building")),
        };
        // Das letzte seiner Art war gratis (das nächste ist es wieder): keine Erstattung.
        let last_free = self.economy.building(b.kind).is_some_and(|d| d.first_free)
            && !self.buildings.iter().any(|o| o.id != b.id && o.owner == player && o.kind == b.kind);
        let pct = if last_free { 0 } else { self.economy.def.demolish_refund_percent };
        let mut refund: Vec<GoodAmount> = Vec::new();
        // Baustelle eines Belagerungsgeräts: was schon geliefert war, kommt zurück.
        if let Some(site) = &b.site {
            refund.extend(site.delivered.iter().copied());
        }
        if let Some(def) = self.economy.building(b.kind) {
            // Brücken kosten je Feld.
            let cells = if b.kind == BuildingKind::Bridge { bridge::bridge_len(&b) } else { 1 };
            for level in def.levels.iter().take(b.level as usize) {
                refund.extend(level.cost.iter().map(|c| GoodAmount { good: c.good, amount: c.amount * cells * pct / 100 }));
            }
        }
        self.remove_building(id);
        if self.economy.building(b.kind).is_some_and(|d| d.hub) {
            self.update_capacity(player);
        }
        if let Some(stock) = self.stocks.get_mut(&player) {
            for g in refund {
                stock.add(g.good, g.amount);
            }
            self.dirty_stocks.insert(player);
        }
        self.replan_all();
        Ok(())
    }

    /// Gebäude entfernen. Was in ihm lag und was seine Träger tragen, kommt in den Vorrat.
    fn remove_building(&mut self, id: u32) {
        let Some(pos) = self.buildings.iter().position(|b| b.id == id) else { return };
        let b = self.buildings.remove(pos);
        if !b.training.is_empty() {
            self.refund_training(b.owner, &b.training, (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0));
        }
        self.work.remove(&id);
        self.dirty_buildings.remove(&id);
        self.entrances.retain(|_, b| *b != id);
        let mut back: Vec<GoodAmount> = self.inventories.remove(&id).map(|s| s.state().goods).unwrap_or_default();
        back.extend(self.units.iter().filter(|u| u.home == Some(id)).filter_map(|u| u.carrying));
        self.units.retain(|u| u.home != Some(id));
        if let Some(stock) = self.stocks.get_mut(&b.owner) {
            for g in back {
                stock.add(g.good, g.amount);
            }
            self.dirty_stocks.insert(b.owner);
        }
        if b.kind == BuildingKind::Bridge {
            self.drown_on_bridge(&b);
        }
        self.set_footprint(&b, false);
        self.drop_from_walls();
    }

    /// Nach einer Änderung am Gitter (Bauen, Abreißen, ...). Wessen Weg jetzt durch ein neu
    /// blockiertes Feld führt, der sucht sofort neu. Alle anderen erst, wenn eine Weile
    /// (`movement.replan_delay_seconds`) nichts mehr geändert wurde: beim Mauerbau kommen viele
    /// Teile kurz hintereinander, und jedes Mal alle Wege neu zu suchen kostet viel.
    fn replan_all(&mut self) {
        let delay = self.economy.def.movement.replan_delay_seconds;
        if delay <= 0.0 {
            self.replan_now();
            return;
        }
        self.replan_blocked();
        let now = self.replan_clock;
        self.replan_pending = Some(match self.replan_pending {
            Some((first, _)) => (first, now),
            None => (now, now),
        });
    }

    /// Aufgeschobene Neuberechnung fällig? (Ruhe seit der letzten Änderung oder doppelte Zeit um.)
    fn step_replan(&mut self, dt: f32) {
        self.replan_clock += dt as f64;
        // Blockaden, nach denen niemand `replan_all` gerufen hat, sind schon erledigt.
        self.map.nav.newly_blocked.clear();
        let Some((first, last)) = self.replan_pending else { return };
        let delay = self.economy.def.movement.replan_delay_seconds as f64;
        if self.replan_clock - last >= delay || self.replan_clock - first >= 2.0 * delay {
            self.replan_now();
        }
    }

    /// Nur die Einheiten, deren restlicher Weg ein eben blockiertes Feld kreuzt, suchen neu.
    fn replan_blocked(&mut self) {
        let cells: HashSet<(i32, i32)> =
            std::mem::take(&mut self.map.nav.newly_blocked).into_iter().filter(|&(x, y)| !self.map.nav.is_open(x, y)).collect();
        if cells.is_empty() {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y) in &cells {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let hits = |from: (f32, f32), to: (f32, f32)| -> bool {
            let (lo_x, hi_x) = (from.0.min(to.0).floor() as i32, from.0.max(to.0).floor() as i32);
            let (lo_y, hi_y) = (from.1.min(to.1).floor() as i32, from.1.max(to.1).floor() as i32);
            if hi_x < x0 || lo_x > x1 || hi_y < y0 || lo_y > y1 {
                return false;
            }
            let len = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
            let steps = (len * 4.0).ceil().max(1.0) as i32;
            (0..=steps).any(|i| {
                let f = i as f32 / steps as f32;
                let p = (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
                cells.contains(&(p.0.floor() as i32, p.1.floor() as i32))
            })
        };
        let affected: Vec<usize> = (0..self.units.len())
            .filter(|&k| {
                let u = &self.units[k];
                let mut at = u.pos;
                u.path.iter().any(|&next| {
                    let hit = hits(at, next);
                    at = next;
                    hit
                })
            })
            .collect();
        for k in affected {
            self.replan_unit(k);
        }
    }

    /// Sofort alle laufenden (und vor einem Hindernis wartenden) Einheiten neu planen.
    fn replan_now(&mut self) {
        self.replan_pending = None;
        self.map.nav.newly_blocked.clear();
        for k in 0..self.units.len() {
            let u = &self.units[k];
            let Some(target) = u.target else { continue };
            // Soldaten, die vor einem Hindernis (z. B. geschlossenem Tor) warten, versuchen es
            // erneut; Träger und Arbeiter nur, solange sie unterwegs sind.
            let waiting = !u.is_civilian() && (u.pos.0 - target.0).abs() + (u.pos.1 - target.1).abs() > 0.05;
            if !u.path.is_empty() || waiting {
                self.replan_unit(k);
            }
        }
    }

    fn replan_unit(&mut self, k: usize) {
        let u = &self.units[k];
        let Some(target) = u.target else { return };
        let (path, layers) = if u.is_civilian() {
            (self.map.nav.waypoints(u.pos, target), Vec::new())
        } else {
            self.plan(u.owner, u.pos, u.on_wall, target, u.target_wall)
        };
        let u = &mut self.units[k];
        u.path = path;
        u.layers = layers;
    }

    /// Sofort anhalten (Soldaten, König): wer läuft, bleibt stehen, Wegpunkte und Angriffsbefehl
    /// verfallen. Fernkämpfer dürfen danach gleich wieder von selbst schießen.
    pub fn order_stop(&mut self, player: u32, unit_ids: &[u32]) {
        for u in self.units.iter_mut().filter(|u| u.owner == player && !u.is_civilian() && unit_ids.contains(&u.id)) {
            u.path.clear();
            u.layers.clear();
            u.waypoints.clear();
            u.attack_order = None;
            u.resume_order = None;
            u.attack_force = false;
            u.chase_timer = 0.0;
            u.speed_cap = None;
            u.mount_at = None;
            u.target = Some(u.pos);
            u.target_wall = u.on_wall;
        }
    }

    /// Bewegungsbefehl eines Spielers. Fremde Einheiten werden ignoriert.
    /// Liefert false, wenn keine der Einheiten das Ziel erreichen kann; dann
    /// läuft auch keine los. Wer nicht hinkommt, bleibt stehen.
    pub fn order_move(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32)) -> bool {
        self.order_move_as(player, unit_ids, target, false)
    }

    /// Wie `order_move`. Eine Gruppe läuft im Tempo ihrer langsamsten Einheit; mit `fast`
    /// (zweiter Befehl gleich hinterher) läuft jede so schnell sie kann.
    pub fn order_move_as(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32), fast: bool) -> bool {
        self.order_move_full(player, unit_ids, target, fast, false)
    }

    /// Wie `order_move_as`. Mit `append` (Strg+Rechtsklick) wird das Ziel hinten an die
    /// Wegpunkte gehängt: wer gerade läuft, geht erst zu Ende und dann weiter. Ohne `append`
    /// verfallen alle alten Wegpunkte.
    pub fn order_move_full(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32), fast: bool, append: bool) -> bool {
        if !target.0.is_finite() || !target.1.is_finite() {
            return false;
        }
        // Feindliche Tore gelten für die Planung immer als zu, auch wenn sie gerade offen
        // stehen. So läuft niemand los zu einem Ziel in einer fremden Burg, in die er gar
        // nicht hineinkommt (wie bei unerreichbaren Zielen).
        let shut: Vec<(i32, i32)> = self
            .buildings
            .iter()
            .filter(|b| gate::is_gate(b.kind) && b.gate_open && !self.allied(player, b.owner))
            .flat_map(gate::passage_tiles)
            .map(|(x, y)| (x as i32, y as i32))
            .collect();
        let noted = self.map.nav.newly_blocked.len();
        for &(x, y) in &shut {
            self.map.nav.set_blocked(x, y, true);
        }
        let ok = self.order_move_planned(player, unit_ids, target, fast, append);
        for &(x, y) in &shut {
            self.map.nav.set_blocked(x, y, false);
        }
        // Nur für diese Planung zu: zählt nicht als Änderung am Gitter.
        self.map.nav.newly_blocked.truncate(noted);
        ok
    }

    fn order_move_planned(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32), fast: bool, append: bool) -> bool {
        let target = (
            target.0.clamp(0.0, self.map.width as f32 - 0.01),
            target.1.clamp(0.0, self.map.height as f32 - 0.01),
        );
        let target_wall = self.is_walkway(player, target);
        // Soldaten, König und eigene Handelskarren (Handel oder Beute) lassen sich schicken.
        let chosen: Vec<usize> = (0..self.units.len())
            .filter(|&k| {
                let u = &self.units[k];
                u.owner == player && (!u.is_civilian() || u.trade.is_some()) && unit_ids.contains(&u.id) && !self.immobile(u)
            })
            .collect();
        // Ein Laufbefehl holt Baumeister von ihrem Gerät weg.
        for &k in &chosen {
            self.units[k].crew_of = None;
        }
        // Reiter kommen nicht auf Wehrgänge: sie stellen sich unten an den Fuß des Bauwerks.
        if target_wall && chosen.iter().any(|&k| self.units[k].mounted) {
            let (riders, walkers): (Vec<usize>, Vec<usize>) = chosen.iter().partition(|&&k| self.units[k].mounted);
            let rider_ids: Vec<u32> = riders.iter().map(|&k| self.units[k].id).collect();
            let walker_ids: Vec<u32> = walkers.iter().map(|&k| self.units[k].id).collect();
            let foot = self.foot_of(target, self.units[riders[0]].pos);
            let rode = !self.is_walkway(player, foot) && self.order_move_planned(player, &rider_ids, foot, fast, append);
            let walked = !walker_ids.is_empty() && self.order_move_planned(player, &walker_ids, target, fast, append);
            return rode || walked;
        }
        let spots = self.formation(player, unit_ids, target, chosen.len(), target_wall);
        if append {
            // Wer noch unterwegs ist (oder schon Wegpunkte hat), merkt sich das Ziel für später.
            let cap = if fast || chosen.len() < 2 {
                None
            } else {
                chosen.iter().map(|&k| self.base_speed(&self.units[k])).min_by(f32::total_cmp)
            };
            let mut queued = false;
            let mut rest = Vec::new();
            for (&k, &spot) in chosen.iter().zip(&spots) {
                let u = &mut self.units[k];
                // Ein Handelskarren auf seinem eigenen Weg zählt als frei: der erste Befehl ersetzt ihn.
                let auto_cart = u.trade.as_ref().is_some_and(|d| !d.steered);
                if auto_cart || (u.path.is_empty() && u.waypoints.is_empty() && u.attack_order.is_none()) {
                    rest.push(k);
                } else {
                    u.waypoints.push(Waypoint { to: spot, wall: target_wall, cap });
                    queued = true;
                }
            }
            if rest.is_empty() {
                return queued;
            }
            let ids: Vec<u32> = rest.iter().map(|&k| self.units[k].id).collect();
            return self.order_move_planned(player, &ids, target, fast, false) || queued;
        }
        // Ein direkter Befehl löscht alle alten Wegpunkte.
        for &k in &chosen {
            self.units[k].waypoints.clear();
        }
        let mut room = if target_wall { self.garrison_room(player, unit_ids, target) } else { None };
        // Turm voll: die übrigen stellen sich unten neben den Turm.
        let mut feet = match (room, chosen.first()) {
            (Some(n), Some(&k)) if n < chosen.len() => self.formation(player, unit_ids, self.foot_of(target, self.units[k].pos), chosen.len() - n, false),
            _ => Vec::new(),
        }
        .into_iter();
        // Ankunft „nah genug“: Klick auf ein Hindernis direkt am Weg zählt noch.
        const REACH: f32 = 1.5;
        let near = |p: (f32, f32), q: (f32, f32)| (p.0 - q.0).hypot(p.1 - q.1) <= REACH;
        // Erst: kommt überhaupt jemand zum geklickten Punkt? Sonst läuft keiner los.
        let reachable = chosen.iter().any(|&k| {
            let u = &self.units[k];
            let route = self.plan(player, u.pos, u.on_wall, target, target_wall);
            near(route.0.last().copied().unwrap_or(u.pos), target)
        });
        if !reachable {
            return false;
        }
        // Ein Laufbefehl hebt einen Angriffsbefehl auf (und den Weg zum Pferd).
        for &k in &chosen {
            self.units[k].attack_order = None;
            self.units[k].resume_order = None;
            self.units[k].mount_at = None;
        }
        let mut orders = Vec::new();
        for (&k, &spot) in chosen.iter().zip(&spots) {
            let u = &self.units[k];
            let (to, wall) = match &mut room {
                Some(0) => (feet.next().unwrap_or(target), false),
                Some(n) => {
                    *n -= 1;
                    (spot, true)
                }
                None => (spot, target_wall),
            };
            let route = self.plan(player, u.pos, u.on_wall, to, wall);
            let end = route.0.last().copied().unwrap_or(u.pos);
            // Am Rand der Formation unerreichbar? Dann zum Zielpunkt selbst.
            let (to, route) = if near(end, to) {
                (to, route)
            } else {
                let direct = self.plan(player, u.pos, u.on_wall, target, wall);
                let end = direct.0.last().copied().unwrap_or(u.pos);
                if !near(end, target) {
                    continue;
                }
                (target, direct)
            };
            orders.push((k, to, wall, route));
        }
        if orders.is_empty() {
            return false;
        }
        // Gruppe: alle im Tempo der langsamsten (bei nur einer oder `fast`: keine Bremse).
        let cap = if fast || orders.len() < 2 {
            None
        } else {
            orders.iter().map(|o| self.base_speed(&self.units[o.0])).min_by(f32::total_cmp)
        };
        for (k, to, wall, (path, layers)) in orders {
            let u = &mut self.units[k];
            u.speed_cap = cap;
            u.path = path;
            u.layers = layers;
            u.target = Some(to);
            u.target_wall = wall;
            if let Some(d) = &mut u.trade {
                d.steered = true;
            }
        }
        true
    }

    /// Angekommen und noch Wegpunkte übrig? Dann zum nächsten. Unerreichbare werden übersprungen.
    fn step_waypoints(&mut self) {
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.waypoints.is_empty() || !u.path.is_empty() || u.attack_order.is_some() {
                continue;
            }
            let (owner, pos, on_wall) = (u.owner, u.pos, u.on_wall);
            let next = self.units[k].waypoints.remove(0);
            let (path, layers) = self.plan(owner, pos, on_wall, next.to, next.wall);
            let u = &mut self.units[k];
            u.speed_cap = next.cap;
            u.path = path;
            u.layers = layers;
            u.target = Some(next.to);
            u.target_wall = next.wall;
        }
    }

    /// Ziele für eine Gruppe: ein enges Raster (`FORMATION_GAP`) um den
    /// Zielpunkt, damit nicht alle auf einem Fleck stehen. Punkte auf nicht
    /// begehbaren Feldern, auf anderer Höhe oder zu nah an einer anderen Einheit
    /// (dort stehend oder auf dem Weg dorthin) werden übersprungen. `wall`: nur
    /// auf dem Wehrgang desselben Bauwerks; auf Türmen innerhalb der runden
    /// Plattform um die Turmmitte.
    fn formation(&self, player: u32, group: &[u32], target: (f32, f32), n: usize, wall: bool) -> Vec<(f32, f32)> {
        let (w, h) = (self.map.width as i32, self.map.height as i32);
        let cell = |p: (f32, f32)| (p.0.floor() as i32, p.1.floor() as i32);
        let others: Vec<(f32, f32)> = self
            .units
            .iter()
            .filter(|u| !u.is_civilian() && !group.contains(&u.id))
            .filter(|u| u.target_wall == wall || u.on_wall == wall)
            .map(|u| u.target.filter(|_| !u.path.is_empty()).unwrap_or(u.pos))
            .collect();
        let level = |x: i32, y: i32| self.map.levels[(y * w + x) as usize];
        let (cx, cy) = cell(target);
        let home = level(cx.clamp(0, w - 1), cy.clamp(0, h - 1));
        let on = self.occupied[(cy.clamp(0, h - 1) * w + cx.clamp(0, w - 1)) as usize];
        // Turm (Besatzung begrenzt): Mitte und Radius der runden Plattform.
        let platform = if wall { self.platform(on) } else { None };
        let center = platform.map_or(target, |p| p.0);
        let fits = |p: (f32, f32)| {
            let (x, y) = cell(p);
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            if wall {
                let on_platform = platform.is_none_or(|(c, r)| (p.0 - c.0).hypot(p.1 - c.1) <= r + 1e-3);
                on_platform && self.occupied[(y * w + x) as usize] == on && self.is_walkway(player, p)
            } else {
                self.map.nav.is_open(x, y) && level(x, y) == home
            }
        };
        let near = |p: (f32, f32), list: &[(f32, f32)]| list.iter().any(|q| (p.0 - q.0).hypot(p.1 - q.1) < FORMATION_GAP * 0.8);
        let mut spots: Vec<(f32, f32)> = Vec::with_capacity(n);
        // Ringe um den Zielpunkt, innen zuerst; erst nur freie Punkte, notfalls auch belegte.
        for free_only in [true, false] {
            'rings: for r in 0..(2 * n as i32 + 4) {
                for dy in -r..=r {
                    for dx in -r..=r {
                        if spots.len() == n {
                            break 'rings;
                        }
                        if dx.abs().max(dy.abs()) != r {
                            continue;
                        }
                        let spot = (center.0 + dx as f32 * FORMATION_GAP, center.1 + dy as f32 * FORMATION_GAP);
                        if !fits(spot) || near(spot, &spots) || (free_only && near(spot, &others)) {
                            continue;
                        }
                        spots.push(spot);
                    }
                }
            }
        }
        while spots.len() < n {
            spots.push(center);
        }
        spots
    }

    /// Turm (Besatzung begrenzt) mit dieser Nummer: Mitte und Radius seiner runden Plattform.
    /// Groß genug für die Besatzung, aber innerhalb der Zinnen.
    fn platform(&self, building: u32) -> Option<((f32, f32), f32)> {
        let b = self.buildings.iter().find(|b| b.id == building)?;
        let crew = self.economy.building(b.kind).map_or(0, |d| d.at(b.level).garrison);
        if crew == 0 {
            return None;
        }
        let side = b.w.min(b.h) as f32;
        let need = FORMATION_GAP * (crew as f32 / std::f32::consts::PI).sqrt() * 1.2;
        Some(((b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0), need.clamp(side * 0.3, side * 0.45)))
    }

    /// Gebäude mit `build_radius` nur im Umkreis des eigenen Bergfrieds:
    /// die ganze Fläche muss drinliegen.
    fn check_build_radius(&self, player: u32, r: u32, at: (u32, u32), size: (u32, u32)) -> Result<(), Refusal> {
        if r == 0 {
            return Ok(());
        }
        let corners = [(at.0, at.1), (at.0 + size.0, at.1), (at.0, at.1 + size.1), (at.0 + size.0, at.1 + size.1)];
        let inside = self.buildings.iter().filter(|b| b.owner == player && b.kind == BuildingKind::Keep).any(|b| {
            let (cx, cy) = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
            corners.iter().all(|&(x, y)| (x as f32 - cx).hypot(y as f32 - cy) <= r as f32)
        });
        if inside {
            Ok(())
        } else {
            Err(Text::new("error.outside_build_radius").arg("r", r))
        }
    }

    /// Wie viele Einheiten noch auf das Gebäude am Ziel passen (`garrison`).
    /// None, wenn es keine Grenze hat.
    fn garrison_room(&self, player: u32, ordered: &[u32], target: (f32, f32)) -> Option<usize> {
        let i = (target.1 as u32 * self.map.width + target.0 as u32) as usize;
        let b = self.buildings.iter().find(|b| b.id == self.occupied[i])?;
        let max = self.economy.building(b.kind)?.at(b.level).garrison as usize;
        if max == 0 {
            return None;
        }
        let inside = |p: (f32, f32)| {
            p.0 >= b.x as f32 && p.1 >= b.y as f32 && p.0 < (b.x + b.w) as f32 && p.1 < (b.y + b.h) as f32
        };
        let taken = self
            .units
            .iter()
            .filter(|u| self.allied(u.owner, player) && !u.is_civilian() && !ordered.contains(&u.id))
            .filter(|u| match u.target {
                Some(t) if u.target_wall => inside(t),
                _ => u.on_wall && inside(u.pos),
            })
            .count();
        Some(max.saturating_sub(taken))
    }

    /// Freies Feld direkt neben dem Gebäude am Punkt `p`, möglichst nah bei `from`.
    pub(super) fn foot_of(&self, p: (f32, f32), from: (f32, f32)) -> (f32, f32) {
        let i = (p.1 as u32 * self.map.width + p.0 as u32) as usize;
        let Some(b) = self.buildings.iter().find(|b| b.id == self.occupied[i]) else { return p };
        let (x0, y0, x1, y1) = (b.x as i32 - 1, b.y as i32 - 1, (b.x + b.w) as i32, (b.y + b.h) as i32);
        (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
            .filter(|&(x, y)| (x == x0 || x == x1 || y == y0 || y == y1) && self.map.nav.is_open(x, y))
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .min_by(|a, c| {
                let d = |q: &(f32, f32)| (q.0 - from.0).powi(2) + (q.1 - from.1).powi(2);
                d(a).total_cmp(&d(c))
            })
            .unwrap_or(p)
    }

    /// Einen Spieltakt von `dt` Sekunden simulieren.
    /// Gespielte Spielzeit in Sekunden.
    pub fn game_seconds(&self) -> f64 {
        self.game_seconds
    }

    pub fn step(&mut self, dt: f32) {
        use crate::profile::Phase;
        macro_rules! timed {
            ($s:ident, $p:expr, $e:expr) => {{
                let t = std::time::Instant::now();
                let r = $e;
                $s.phase_us.add($p, t.elapsed());
                r
            }};
        }
        timed!(self, Phase::People, self.step_people(dt));
        timed!(self, Phase::Production, self.step_production(dt));
        timed!(self, Phase::Transport, self.step_transport(dt));
        timed!(self, Phase::Gather, self.step_gather(dt));
        timed!(self, Phase::Progress, self.step_progress(dt));
        timed!(self, Phase::Gates, self.step_gates());
        timed!(self, Phase::Army, self.step_army(dt));
        timed!(self, Phase::Horses, self.step_horses(dt));
        timed!(self, Phase::Trade, self.step_trade(dt));
        timed!(self, Phase::Trade, self.step_auto_trade(dt));
        timed!(self, Phase::Combat, self.step_siege(dt));
        timed!(self, Phase::Combat, self.step_combat(dt));
        timed!(self, Phase::Waypoints, self.step_waypoints());
        timed!(self, Phase::Heal, self.step_heal(dt));
        timed!(self, Phase::Alerts, self.step_alerts(dt));
        timed!(self, Phase::Orchards, self.step_orchards(dt));
        timed!(self, Phase::Food, self.spoil_unstored_food());
        timed!(self, Phase::Replan, self.step_replan(dt));
        if self.winners.is_none() {
            self.game_seconds += dt as f64;
        }
        timed!(self, Phase::Hubs, self.balance_hubs());
        let moving = std::time::Instant::now();
        let t = &self.economy.def.transport;
        let (carrier, cart) = (t.carrier_speed, t.cart_speed);
        let king = self.economy.def.movement.king_speed;
        let worker = self.economy.def.movement.worker_speed;
        let horse = self.economy.def.army.horse_speed;
        let civil: Vec<f32> = (0..self.units.len()).map(|k| self.unit_hunger_factor(k)).collect();
        let soldier: Vec<f32> = self.units.iter().map(|u| if u.kind.is_soldier() { self.soldier_speed(u.owner, u.kind) } else { 0.0 }).collect();
        // Belagerungsgeräte fahren nur mit voller Bedienung.
        let crews = self.crew_counts();
        let siege: Vec<f32> = self
            .units
            .iter()
            .map(|u| if u.kind.is_siege() && !self.immobile(u) && self.crewed(u, &crews) { self.soldier_speed(u.owner, u.kind) } else { 0.0 })
            .collect();
        let nav = &self.map.nav;
        for (k, unit) in self.units.iter_mut().enumerate() {
            let full = match unit.kind {
                UnitKind::King => king,
                UnitKind::Carrier => carrier * civil[k],
                UnitKind::Cart | UnitKind::TradeCart => cart * civil[k],
                UnitKind::Worker => worker * civil[k],
                UnitKind::Spearman | UnitKind::Archer | UnitKind::Crossbowman | UnitKind::LightSwordsman | UnitKind::Swordsman | UnitKind::Knight | UnitKind::Healer | UnitKind::Builder => soldier[k],
                UnitKind::Catapult | UnitKind::Trebuchet | UnitKind::Ballista => siege[k],
            };
            // Zu Pferd mindestens so schnell wie das Pferd; nach einem Sturz liegt man still.
            let full = if unit.down > 0.0 { 0.0 } else if unit.mounted { full.max(horse) } else { full };
            let speed = unit.speed_cap.map_or(full, |c| full.min(c));
            if unit.path.is_empty() {
                unit.speed_cap = None;
            }
            // Auf Wegen schneller (oben auf der Mauer zählt der Boden nicht).
            let road = if unit.on_wall { 1.0 } else { nav.speed_at(unit.pos) };
            let mut budget = speed * road * dt;
            let before = unit.pos;
            while budget > 0.0 {
                let Some(&next) = unit.path.first() else { break };
                let (dx, dy) = (next.0 - unit.pos.0, next.1 - unit.pos.1);
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= budget {
                    unit.pos = next;
                    unit.path.remove(0);
                    if !unit.layers.is_empty() {
                        unit.on_wall = unit.layers.remove(0);
                    }
                    budget -= dist;
                } else {
                    unit.pos.0 += dx / dist * budget;
                    unit.pos.1 += dy / dist * budget;
                    budget = 0.0;
                }
            }
            unit.vel = if dt > 0.0 { ((unit.pos.0 - before.0) / dt, (unit.pos.1 - before.1) / dt) } else { (0.0, 0.0) };
        }
        self.phase_us.add(Phase::Movement, moving.elapsed());
        timed!(self, Phase::Spread, self.step_spread(dt));
        timed!(self, Phase::Buildings, self.flush_buildings());
    }

    /// Geänderte Gebäude (Status, Lager) an alle Clients melden.
    fn flush_buildings(&mut self) {
        for id in std::mem::take(&mut self.dirty_buildings) {
            let (goods, capacity) = match self.inventories.get(&id) {
                Some(s) => (s.state().goods, 0),
                None => (self.hub_state(id).unwrap_or_default(), self.hub_capacity(id)),
            };
            let Some(b) = self.buildings.iter_mut().find(|b| b.id == id) else { continue };
            // Pferdezucht: Pferde zählen zum angezeigten Vorrat.
            if self.economy.building(b.kind).is_some_and(|d| d.horse_capacity > 0) {
                self.dirty_stocks.insert(b.owner);
            }
            b.inventory = goods;
            b.capacity = capacity;
            self.events.push(ServerMsg::BuildingChanged { building: b.clone() });
        }
    }

    /// Besitzer einer Einheit.
    pub fn unit_owner(&self, id: u32) -> Option<u32> {
        self.units.iter().find(|u| u.id == id).map(|u| u.owner)
    }

    /// Restlicher Weg einer Einheit (Wegpunkte).
    pub fn unit_path(&self, id: u32) -> Option<Vec<(f32, f32)>> {
        self.units.iter().find(|u| u.id == id).map(|u| u.path.iter().copied().chain(u.waypoints.iter().map(|w| w.to)).collect())
    }

    pub fn snapshot(&self) -> Vec<UnitState> {
        let crews = self.crew_counts();
        self.units
            .iter()
            .map(|u| UnitState {
                id: u.id,
                owner: u.owner,
                kind: u.kind,
                x: u.pos.0,
                y: u.pos.1,
                moving: !u.path.is_empty(),
                carrying: u.carrying.map(|g| g.good),
                on_wall: u.on_wall,
                inside: self.inside(u),
                hp: self.unit_hp(u),
                mounted: u.mounted,
                horse_hp: if u.mounted { self.economy.def.army.horse_hp.saturating_sub(u.horse_damage) } else { 0 },
                down: u.down,
                attack_target: u.attack_order.or(u.last_foe.map(|(id, _)| protocol::AttackTarget::Unit { id })),
                strikes: u.strikes,
                crew: if u.kind.is_siege() { crews.get(&u.id).copied().unwrap_or(0) } else { 0 },
                crew_of: u.crew_of,
                shots_left: self.economy.unit(u.kind).and_then(|d| d.siege.as_ref()).filter(|s| s.shots > 0).map(|s| s.shots.saturating_sub(u.shots_fired)),
            })
            .collect()
    }

    /// Was eine Einheit tut, wohin sie will und was sie trägt (für die Anzeige).
    pub fn unit_info(&self, id: u32) -> Option<UnitInfo> {
        let u = self.units.iter().find(|u| u.id == id)?;
        let walking = !u.path.is_empty();
        let mut target_building = None;
        let mut job_good = u.carrying.map(|g| g.good);
        let activity = match (&u.gather, &u.job) {
            (Some(Gather::ToTile(_)), _) => UnitActivity::ToResource,
            (Some(Gather::Working { .. }), _) => UnitActivity::Gathering,
            (Some(Gather::Home), _) => {
                target_building = u.home;
                UnitActivity::BringingHome
            }
            (Some(Gather::Deliver(b)), _) => {
                target_building = Some(*b);
                UnitActivity::Delivering
            }
            (Some(Gather::Fetch(b)), _) => {
                target_building = Some(if u.carrying.is_some() { u.home.unwrap_or(*b) } else { *b });
                UnitActivity::Fetching
            }
            (None, Some(job)) => {
                let (loading, building, good) = job.describe();
                target_building = Some(building);
                job_good = Some(good);
                if loading { UnitActivity::PickingUp } else { UnitActivity::Unloading }
            }
            (None, None) if self.inside(u) => UnitActivity::Working,
            (None, None) if walking && u.is_civilian() => {
                target_building = u.home;
                UnitActivity::Returning
            }
            (None, None) if u.down > 0.0 => UnitActivity::Fallen,
            (None, None) if u.mount_at.is_some() => {
                target_building = u.mount_at;
                UnitActivity::ToHorse
            }
            (None, None) if walking => UnitActivity::Walking,
            (None, None) if u.on_wall => UnitActivity::OnWall,
            (None, None) => UnitActivity::Idle,
        };
        Some(UnitInfo {
            unit_id: u.id,
            activity,
            home: u.home,
            target_building,
            target: u.path.last().copied(),
            job_good,
            carrying: u.carrying,
        })
    }

    /// Arbeiter steht ohne Weg und Last an seinem Gebäude: er ist drinnen.
    fn inside(&self, u: &Unit) -> bool {
        // Ohne Wohnung gibt es den Arbeiter (oder Träger) gar nicht: nicht zeigen.
        if u.kind == UnitKind::Worker && u.home.is_some_and(|h| self.unstaffed.contains(&h)) {
            return true;
        }
        if u.is_transport() && self.idle_units.contains(&u.id) && u.path.is_empty() && u.carrying.is_none() {
            return true;
        }
        if u.kind != UnitKind::Worker || !u.path.is_empty() || u.carrying.is_some() {
            return false;
        }
        if matches!(u.gather, Some(gather::Gather::Working { .. } | gather::Gather::ToTile(_))) {
            return false;
        }
        let Some(b) = u.home.and_then(|h| self.buildings.iter().find(|b| b.id == h)) else { return false };
        self.at_door(b, u.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::DEFAULT_MAP;
    use protocol::Good;

    #[test]
    fn einheiten_info_zeigt_taetigkeit_und_ziel() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap(), eco());
        world.spawn_player(1);
        let id = world.units[0].id;
        assert_eq!(world.unit_info(id).unwrap().activity, UnitActivity::Idle);
        world.order_move(1, &[id], (122.5, 82.5));
        let info = world.unit_info(id).unwrap();
        assert_eq!(info.activity, UnitActivity::Walking);
        let (tx, ty) = info.target.unwrap();
        assert!((tx - 122.5).abs() < 1.5 && (ty - 82.5).abs() < 1.5, "Ziel {tx},{ty}");
        assert!(world.unit_info(999_999).is_none());
    }

    #[test]
    fn ohne_wohnung_kein_arbeiter_zu_sehen() {
        let row = "0".repeat(40);
        let text = format!("spawn 0 19\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building());
        w.spawn_player(1);
        w.admin_give(1, &[GoodAmount { good: protocol::Good::Wood, amount: 500 }], 0);
        let saw = w.place_building(1, BuildingKind::Woodcutter, 20, 2).unwrap();
        w.step(0.05);
        assert_eq!(w.buildings.iter().find(|b| b.id == saw.id).unwrap().status, BuildingStatus::NoWorkers);
        let worker = w.units.iter().find(|u| u.home == Some(saw.id)).unwrap().id;
        assert!(w.snapshot().iter().find(|u| u.id == worker).unwrap().inside, "unbesetzt: Arbeiter unsichtbar");
    }

    #[test]
    fn koenig_laeuft_aus_der_burg_ins_tal() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap(), eco());
        world.spawn_player(1);
        world.spawn_player(2);
        let id = world.units[0].id;
        world.order_move(1, &[id], (122.5, 82.5));
        world.order_move(1, &[world.units[1].id], (0.5, 0.5)); // fremde Einheit: ignoriert
        assert!(world.units[1].path.is_empty());
        for _ in 0..20 * 120 {
            world.step(0.05);
            let u = &world.units[0];
            let (x, y) = (u.pos.0.floor() as i32, u.pos.1.floor() as i32);
            assert!(world.map.nav.is_open(x, y), "König steht auf ({x}, {y}) im Wasser/Fels");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 122.5).abs() < 1e-3 && (u.pos.1 - 82.5).abs() < 1e-3, "König bei {:?}", u.pos);
    }

    fn eco() -> Economy {
        Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people().without_build_radius()
    }

    fn flat_world() -> World {
        let row = "0".repeat(24);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(24));
        let mut world = World::new(GameMap::parse(&text).unwrap(), eco());
        world.spawn_player(1);
        world
    }

    #[test]
    fn strg_rechtsklick_haengt_wegpunkte_an() {
        let mut w = flat_world();
        let id = w.units.iter().find(|u| u.kind == UnitKind::King).unwrap().id;
        assert!(w.order_move(1, &[id], (20.5, 2.5)));
        assert!(w.order_move_full(1, &[id], (20.5, 20.5), false, true));
        assert!(w.order_move_full(1, &[id], (2.5, 20.5), false, true));
        assert_eq!(w.unit_path(id).unwrap().last().copied(), Some((2.5, 20.5)), "Weg zeigt alle Wegpunkte");
        let mut seen = [false; 3];
        for _ in 0..20 * 120 {
            w.step(0.05);
            let p = w.units.iter().find(|u| u.id == id).unwrap().pos;
            for (k, q) in [(20.5, 2.5), (20.5, 20.5), (2.5, 20.5)].into_iter().enumerate() {
                if (p.0 - q.0).hypot(p.1 - q.1) < 0.5 && (k == 0 || seen[k - 1]) {
                    seen[k] = true;
                }
            }
        }
        assert_eq!(seen, [true; 3], "der Reihe nach abgelaufen");
        // Ein normaler Befehl löscht die alten Wegpunkte.
        w.order_move(1, &[id], (20.5, 20.5));
        w.order_move_full(1, &[id], (20.5, 2.5), false, true);
        w.order_move(1, &[id], (10.5, 10.5));
        assert_eq!(w.unit_path(id).unwrap().last().copied(), Some((10.5, 10.5)));
        assert!(w.units.iter().find(|u| u.id == id).unwrap().waypoints.is_empty());
    }

    #[test]
    fn gebaeude_versetzen_kostet_gebuehr() {
        let mut w = flat_world();
        let saw = w.place_building(1, BuildingKind::Sawmill, 4, 4).unwrap();
        w.inventories.get_mut(&saw.id).unwrap().add(Good::Boards, 3);
        let cost = w.move_cost(&saw);
        assert!(!cost.is_empty());
        let before: Vec<u32> = cost.iter().map(|c| w.stocks[&1].amount(c.good)).collect();
        // Ein Feld weiter: überlappt die alte Fläche, geht trotzdem.
        let moved = w.move_building(1, saw.id, 5, 4, 1).unwrap();
        assert_eq!((moved.x, moved.y, moved.rotation), (5, 4, 1));
        for (c, b) in cost.iter().zip(before) {
            assert_eq!(w.stocks[&1].amount(c.good), b - c.amount, "Gebühr bezahlt");
        }
        assert!(w.map.nav.is_open(4, 4), "alte Fläche frei");
        assert!(!w.map.nav.is_open(5, 4), "neue Fläche belegt");
        assert_eq!(w.inventories[&saw.id].amount(Good::Boards), 3, "Inhalt bleibt");
        assert_eq!(w.place_building(1, BuildingKind::Wall, 5, 5).unwrap_err().key, "error.occupied");
        w.place_building(1, BuildingKind::Wall, 4, 4).unwrap();
        // Nicht auf ein anderes Gebäude.
        assert_eq!(w.move_building(1, saw.id, 3, 3, 0).unwrap_err().key, "error.occupied");
        assert!(!w.map.nav.is_open(5, 4), "nach Fehlschlag steht es noch da");
        assert_eq!(w.move_building(2, saw.id, 12, 12, 0).unwrap_err().key, "error.not_yours");
    }

    #[test]
    fn leichte_schraege_unter_wenigen_feldern_erlaubt() {
        let mut w = flat_world();
        w.economy.def.slope_tiles = 2;
        let mw = w.map.width;
        let def = w.economy.building(BuildingKind::House).unwrap().clone();
        assert!(!def.on_ramps);
        let (x, y) = (10u32, 10u32);
        let size = (def.w, def.h);
        // Eine Ecke eine Stufe höher: geht (1 Feld).
        w.map.levels[(y * mw + x) as usize] += 1;
        assert!(w.check_site(&def, (x, y), size, 0, &[]).is_ok());
        assert_eq!(w.base_level((x, y), size), w.map.levels[((y + 1) * mw + x + 1) as usize], "Grundhöhe = Mehrheit");
        // Zweites Feld: geht noch.
        w.map.levels[(y * mw + x + 1) as usize] += 1;
        assert!(w.check_site(&def, (x, y), size, 0, &[]).is_ok());
        // Drittes Feld: zu viel.
        w.map.levels[(y * mw + x + 2) as usize] += 1;
        assert_eq!(w.check_site(&def, (x, y), size, 0, &[]).unwrap_err().key, "error.not_flat");
        // Zwei Stufen Unterschied geht nie.
        w.map.levels[(y * mw + x + 2) as usize] -= 1;
        w.map.levels[(y * mw + x) as usize] += 1;
        assert_eq!(w.check_site(&def, (x, y), size, 0, &[]).unwrap_err().key, "error.not_flat");
        w.map.levels[(y * mw + x) as usize] -= 1;
        w.economy.def.slope_tiles = 0;
        assert_eq!(w.check_site(&def, (x, y), size, 0, &[]).unwrap_err().key, "error.not_flat", "0 = ganz eben");
    }

    #[test]
    fn bauregeln_werden_geprueft() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap(), eco());
        world.spawn_player(1);
        let w = world.map.width;
        let water = (0..w * world.map.height).find(|&i| world.map.tiles[i as usize] == TileKind::Water).unwrap();
        assert!(world.place_building(1, BuildingKind::Wall, water % w, water / w).is_err());
        let ramp = (0..w * world.map.height).find(|&i| world.map.tiles[i as usize] == TileKind::Ramp).unwrap();
        world.economy.def.slope_tiles = 0;
        assert_eq!(world.place_building(1, BuildingKind::House, ramp % w, ramp / w).unwrap_err().key, "error.on_ramp");
        // Mauern (`on_ramps`) dürfen auf Schrägen.
        assert!(world.economy.building(BuildingKind::Wall).unwrap().on_ramps);
        let r = world.place_building(1, BuildingKind::Wall, ramp % w, ramp / w);
        assert!(r.as_ref().map_or_else(|e| e.key != "error.on_ramp" && e.key != "error.not_flat", |_| true), "{r:?}");
        assert!(world.place_building(1, BuildingKind::Keep, w - 2, w - 2).is_err(), "ragt aus der Karte");
        let king = world.units[0].pos;
        assert!(world.place_building(1, BuildingKind::Wall, king.0 as u32, king.1 as u32).is_err());

        let mut flat = flat_world();
        let b = flat.place_building(1, BuildingKind::Tower, 4, 4).unwrap();
        assert_eq!((b.w, b.h), (3, 3));
        assert!(flat.place_building(1, BuildingKind::Wall, 5, 5).is_err(), "überlappt");
        assert!(!flat.map.nav.is_open(5, 5));
        flat.place_building(1, BuildingKind::Keep, 10, 10).unwrap();
        assert!(flat.place_building(1, BuildingKind::Keep, 0, 12).is_err(), "nur ein Bergfried");
        assert!(flat.demolish(2, b.id).is_err(), "fremdes Gebäude");
        flat.demolish(1, b.id).unwrap();
        assert!(flat.map.nav.is_open(5, 5));
    }

    #[test]
    fn bauen_kostet_und_abriss_erstattet() {
        use protocol::Good;
        let mut w = flat_world();
        // Feste Erstattung, unabhängig vom Balancing.
        w.economy.def.demolish_refund_percent = 50;
        w.place_building(1, BuildingKind::Storehouse, 14, 14).unwrap(); // Platz für die Erstattung
        let wood = w.stocks[&1].amount(Good::Wood);
        let b = w.place_building(1, BuildingKind::Bowyer, 4, 4).unwrap();
        let s = &w.stocks[&1];
        let cost = w.economy.building(BuildingKind::Bowyer).unwrap().levels[0].cost.iter().find(|c| c.good == Good::Wood).unwrap().amount;
        assert_eq!(s.amount(Good::Wood), wood - cost);
        assert_eq!(b.level, 1);
        // Stufe 2 braucht Eisen, das es zu Beginn nicht gibt.
        let err = w.upgrade(1, b.id).unwrap_err();
        assert_eq!(err.key, "error.not_enough");
        assert_eq!(err.args["good"], "iron");
        w.stocks.get_mut(&1).unwrap().add(Good::Iron, 10);
        assert_eq!(w.upgrade(1, b.id).unwrap().level, 2);
        assert_eq!(w.upgrade(1, b.id).unwrap_err().key, "error.max_level");
        assert_eq!(w.upgrade(2, b.id).unwrap_err().key, "error.not_yours");
        let boards = w.stocks[&1].amount(protocol::Good::Boards);
        w.demolish(1, b.id).unwrap();
        // Hälfte der Bretter von Stufe 1 und 2 zurück (Kosten aus economy.json).
        let back: u32 = w.economy.building(BuildingKind::Bowyer).unwrap().levels[..2]
            .iter()
            .flat_map(|l| &l.cost)
            .filter(|c| c.good == Good::Boards)
            .map(|c| c.amount * 50 / 100)
            .sum();
        assert_eq!(w.stocks[&1].amount(protocol::Good::Boards), boards + back);
        assert!(!w.take_dirty_stocks().is_empty());
        assert!(w.take_dirty_stocks().is_empty(), "nur einmal melden");
    }

    #[test]
    fn ohne_material_kein_bau_und_lager_erhoeht_platz() {
        use protocol::Good;
        let mut w = flat_world();
        let cap = w.stocks[&1].state().capacity;
        w.place_building(1, BuildingKind::Storehouse, 6, 6).unwrap();
        assert_eq!(w.stocks[&1].state().capacity, cap + w.economy.def.storehouse_capacity);
        let all = w.stocks[&1].amount(Good::Stone);
        w.stocks.get_mut(&1).unwrap().take_all(&[GoodAmount { good: Good::Stone, amount: all }]);
        assert_eq!(w.place_building(1, BuildingKind::Wall, 0, 9).unwrap_err().key, "error.not_enough");
    }

    #[test]
    fn drehen_und_eingang_freihalten() {
        let mut w = flat_world();
        // Sägewerk 4x4, Eingang unten mittig.
        let b = w.place_building(1, BuildingKind::Sawmill, 4, 4).unwrap();
        assert_eq!(b.entrance, Some((6, 8)));
        assert_eq!(w.place_building(1, BuildingKind::Wall, 6, 8).unwrap_err().key, "error.blocks_entrance");
        w.place_building(1, BuildingKind::Wall, 5, 8).unwrap();
        // Eingang an den Kartenrand: geht nicht. Gedreht (Eingang links) schon.
        assert_eq!(w.place_building(1, BuildingKind::Sawmill, 12, 20).unwrap_err().key, "error.entrance_blocked");
        let r = w.place_building_rotated(1, BuildingKind::Sawmill, 12, 20, 1, 1).unwrap();
        assert_eq!((r.rotation, r.entrance), (1, Some((11, 22))));
        // Tower ohne Eingang darf direkt anschließen.
        let t = w.place_building(1, BuildingKind::Tower, 16, 20).unwrap();
        assert_eq!(t.entrance, None);
        // Formel für andere Seiten.
        assert_eq!(entrance_tile(0, 0, 4, 2, 3), (4, 0));
        assert_eq!(entrance_tile(5, 5, 3, 3, 2), (6, 4));
        w.demolish(1, b.id).unwrap();
        w.place_building(1, BuildingKind::Wall, 6, 8).unwrap();
    }

    #[test]
    fn gleich_hoehere_stufe_bauen() {
        let mut w = flat_world();
        let def = w.economy.building(BuildingKind::Bowyer).unwrap().clone();
        let cost = level_cost(&def, 2);
        let boards = cost.iter().find(|c| c.good == protocol::Good::Boards).unwrap().amount;
        assert_eq!(boards, def.levels[0].cost.iter().chain(&def.levels[1].cost).filter(|c| c.good == protocol::Good::Boards).map(|c| c.amount).sum::<u32>());
        w.admin_give(1, &cost, 0);
        let before = w.stock_state(1).goods.iter().find(|g| g.good == protocol::Good::Boards).unwrap().amount;
        let b = w.place_building_rotated(1, BuildingKind::Bowyer, 4, 4, 0, 2).unwrap();
        assert_eq!(b.level, 2);
        let after = w.stock_state(1).goods.iter().find(|g| g.good == protocol::Good::Boards).map_or(0, |g| g.amount);
        assert_eq!(before - after, boards);
        assert_eq!(w.place_building_rotated(1, BuildingKind::Bowyer, 12, 4, 0, 3).unwrap_err().key, "error.max_level");
    }

    /// Nicht ineinander (mindestens ein halbes Feld Abstand), aber eng beim Ziel.
    fn assert_spread(pos: &[(f32, f32)], target: (f32, f32), max: f32) {
        for (i, a) in pos.iter().enumerate() {
            assert!((a.0 - target.0).abs() <= max + 1e-3 && (a.1 - target.1).abs() <= max + 1e-3, "zu weit weg: {a:?}");
            for b in &pos[i + 1..] {
                assert!((a.0 - b.0).hypot(a.1 - b.1) >= 0.5, "ineinander: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn gruppe_verteilt_sich_um_das_ziel() {
        let mut w = flat_world();
        let ids: Vec<u32> = (0..5).map(|n| {
            let id = 500 + n;
            w.units.push(Unit::new(id, 1, UnitKind::Spearman, (2.5 + n as f32, 20.5)));
            id
        }).collect();
        w.order_move(1, &ids, (12.5, 12.5));
        for _ in 0..20 * 30 {
            w.step(0.05);
        }
        let pos: Vec<(f32, f32)> = w.units.iter().filter(|u| ids.contains(&u.id)).map(|u| u.pos).collect();
        assert_spread(&pos, (12.5, 12.5), 1.0);
    }

    #[test]
    fn einzeln_zum_selben_ziel_stehen_nicht_ineinander() {
        let mut w = flat_world();
        let ids: Vec<u32> = (0..4).map(|n| {
            let id = 600 + n;
            w.units.push(Unit::new(id, 1, UnitKind::Spearman, (2.5 + n as f32, 20.5)));
            id
        }).collect();
        // Nacheinander einzeln geschickt, alle zum selben Punkt.
        for &id in &ids {
            w.order_move(1, &[id], (12.5, 12.5));
        }
        for _ in 0..20 * 30 {
            w.step(0.05);
        }
        let pos: Vec<(f32, f32)> = w.units.iter().filter(|u| ids.contains(&u.id)).map(|u| u.pos).collect();
        assert_spread(&pos, (12.5, 12.5), 1.0);
        // Wer schon steht, bleibt; ein neuer Befehl dorthin weicht aus.
        w.order_move(1, &[ids[0]], (12.5, 12.5));
        let first = w.units.iter().find(|u| u.id == ids[0]).unwrap();
        assert_eq!(first.target, Some((12.5, 12.5)), "eigenes Feld zählt nicht als belegt");
    }

    #[test]
    fn befestigung_nur_im_umkreis_des_bergfrieds() {
        let row = "0".repeat(80);
        let text = format!("spawn 0 0\n{}", format!("{row}\n").repeat(20));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_start_building();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.admin_give(1, &[GoodAmount { good: protocol::Good::Stone, amount: 500 }, GoodAmount { good: protocol::Good::Wood, amount: 500 }], 0);
        assert_eq!(w.place_building(1, BuildingKind::Wall, 10, 10).unwrap_err().key, "error.outside_build_radius", "ohne Bergfried");
        w.place_building(1, BuildingKind::Keep, 4, 4).unwrap();
        let r = w.economy.building(BuildingKind::Wall).unwrap().build_radius;
        assert!(r > 0);
        w.place_building(1, BuildingKind::Wall, 10, 10).unwrap();
        assert_eq!(w.place_building(1, BuildingKind::Wall, 7 + r, 7).unwrap_err().key, "error.outside_build_radius");
        w.place_building(1, BuildingKind::Wall, 5 + r, 7).unwrap();
        // Ohne eigenen Umkreis (0) dürfen Gebäude beliebig weit weg.
        assert_eq!(w.economy.building(BuildingKind::House).unwrap().build_radius, 0);
        w.place_building(1, BuildingKind::House, 10 + r, 2).unwrap();
    }

    #[test]
    fn bergfried_steht_an_der_fahne() {
        let row = "0".repeat(40);
        let text = format!("spawn 20 10\nspawn 5 5\n{}", format!("{row}\n").repeat(20));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        let keep = w.buildings.iter().find(|b| b.owner == 1 && b.kind == BuildingKind::Keep).expect("Bergfried gesetzt");
        let (cx, cy) = (keep.x + keep.w / 2, keep.y + keep.h / 2);
        assert_eq!((cx, cy), (20, 10), "mittig auf der Fahne");
        let king = w.units.iter().find(|u| u.owner == 1 && u.kind == UnitKind::King).unwrap();
        let (kx, ky) = (king.pos.0 as u32, king.pos.1 as u32);
        assert!(!(kx >= keep.x && kx < keep.x + keep.w && ky >= keep.y && ky < keep.y + keep.h), "König nicht im Bergfried");
        // Fahne nah am Rand: der Bergfried rückt nach innen.
        w.spawn_player(2);
        assert!(w.buildings.iter().any(|b| b.owner == 2 && b.kind == BuildingKind::Keep));
        // Kein zweiter Bergfried.
        assert!(w.place_building(1, BuildingKind::Keep, 30, 2).is_err());
    }

    #[test]
    fn bergfried_kommt_auch_mit_eigenem_bauradius() {
        // Online-Balancing: der Bergfried selbst hat einen Bauradius. Den ersten setzt das Spiel trotzdem.
        let mut v: serde_json::Value = serde_json::from_str(crate::economy::ECONOMY_JSON).unwrap();
        for b in v["buildings"].as_array_mut().unwrap() {
            if b["kind"] == "keep" {
                b["build_radius"] = 30.into();
            }
        }
        let eco = Economy::parse(&v.to_string()).unwrap().without_people();
        let row = "0".repeat(40);
        let text = format!("spawn 20 10\n{}", format!("{row}\n").repeat(20));
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        assert!(w.buildings.iter().any(|b| b.owner == 1 && b.kind == BuildingKind::Keep), "Bergfried gesetzt");
    }

    #[test]
    fn bergfried_hat_platz_fuer_die_besatzung() {
        let row = "0".repeat(40);
        let text = format!("spawn 20 10\n{}", format!("{row}\n").repeat(20));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        let keep = w.buildings.iter().find(|b| b.owner == 1 && b.kind == BuildingKind::Keep).unwrap();
        let center = (keep.x as f32 + keep.w as f32 / 2.0, keep.y as f32 + keep.h as f32 / 2.0);
        let max = w.economy.building(BuildingKind::Keep).unwrap().at(1).garrison as usize;
        assert!(max >= 40, "deutlich mehr Platz als früher (16)");
        let spots = w.formation(1, &[], center, max, true);
        for (i, p) in spots.iter().enumerate() {
            assert!(w.is_walkway(1, *p), "nicht auf dem Wehrgang: {p:?}");
            assert!(spots[i + 1..].iter().all(|q| (p.0 - q.0).hypot(p.1 - q.1) >= 0.5), "ineinander: {p:?}");
        }
    }

    #[test]
    fn nicht_auf_wald_bauen() {
        let text = "spawn 0 0\n[terrain]\n000\n000\n[resources]\n..W\n...\n";
        let mut w = World::new(GameMap::parse(text).unwrap(), eco());
        w.spawn_player(1);
        assert_eq!(w.place_building(1, BuildingKind::Wall, 2, 0).unwrap_err().key, "error.on_resource");
        assert!(w.place_building(1, BuildingKind::Wall, 2, 1).is_ok());
    }

    #[test]
    fn auf_wegen_schneller_und_der_weg_wird_genommen() {
        // König bei (1.5, 1.5), Ziel (20.5, 1.5); ein Steinweg liegt einen Umweg weiter unten.
        let run = |road: bool| {
            let mut w = flat_world();
            w.admin_give(1, &[GoodAmount { good: protocol::Good::Stone, amount: 500 }], 0);
            if road {
                for x in 1..=21 {
                    w.place_building(1, BuildingKind::StonePath, x, 3).unwrap();
                }
                for y in [2, 1] {
                    w.place_building(1, BuildingKind::StonePath, 21, y).unwrap();
                }
                w.place_building(1, BuildingKind::StonePath, 1, 2).unwrap();
            }
            let id = w.units[0].id;
            assert!(w.order_move(1, &[id], (20.5, 1.5)));
            let mut t = 0.0;
            let mut via_road = false;
            while (w.units[0].pos.0 - 20.5).abs() + (w.units[0].pos.1 - 1.5).abs() > 0.01 && t < 60.0 {
                w.step(0.05);
                t += 0.05;
                via_road |= w.units[0].pos.1 > 3.0;
            }
            (t, via_road)
        };
        let (plain, _) = run(false);
        let (fast, via_road) = run(true);
        assert!(via_road, "nimmt den Weg");
        assert!(fast < plain, "mit Weg {fast} s, ohne {plain} s");
        // Wege blockieren nicht: man kann darüber laufen und davor bauen.
        let mut w = flat_world();
        w.admin_give(1, &[GoodAmount { good: protocol::Good::Stone, amount: 50 }], 0);
        w.place_building(1, BuildingKind::StonePath, 5, 5).unwrap();
        assert!(w.map.nav.is_open(5, 5));
        assert!(w.economy.building(BuildingKind::StonePath).unwrap().is_road());
    }

    #[test]
    fn weg_im_nachbargebiet_wird_genommen() {
        // Start und Ziel im oberen Kartenstreifen, der Weg liegt knapp darunter im
        // nächsten Suchgebiet (16er-Blöcke).
        let row = "0".repeat(64);
        let text = format!("spawn 2 14\nspawn 4 14\n{}", format!("{row}\n").repeat(40));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_start_building().without_build_radius();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.admin_give(1, &[GoodAmount { good: protocol::Good::Stone, amount: 500 }], 0);
        for x in 2..=60 {
            w.place_building(1, BuildingKind::StonePath, x, 17).unwrap();
        }
        let id = w.units[0].id;
        assert!(w.order_move(1, &[id], (60.5, 14.5)));
        let mut via_road = false;
        for _ in 0..20 * 60 {
            w.step(0.05);
            via_road |= w.units[0].pos.1 >= 17.0;
        }
        assert!(via_road, "Weg im Nachbargebiet nicht genommen");
    }

    #[test]
    fn unerreichbares_ziel_niemand_laeuft_los() {
        // Links Land, in der Mitte Wasser, rechts eine Insel.
        let row = format!("{}{}{}", "0".repeat(8), "~".repeat(4), "0".repeat(8));
        let text = format!("spawn 2 2\nspawn 3 3\n{}", format!("{row}\n").repeat(10));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_start_building();
        let mut world = World::new(GameMap::parse(&text).unwrap(), eco);
        world.spawn_player(1);
        let id = world.units[0].id;
        let start = world.units[0].pos;
        assert!(!world.order_move(1, &[id], (15.5, 5.5)), "Insel");
        assert!(!world.order_move(1, &[id], (9.5, 5.5)), "Wasser");
        world.step(0.5);
        assert_eq!(world.units[0].pos, start, "nicht losgelaufen");
        assert!(world.order_move(1, &[id], (6.5, 5.5)));
        world.step(0.5);
        assert_ne!(world.units[0].pos, start);
    }

    #[test]
    fn mauer_im_weg_wird_umgangen() {
        let mut world = flat_world();
        let id = world.units[0].id;
        world.order_move(1, &[id], (10.5, 1.5));
        world.step(0.05);
        // Quer über den Weg eine Mauer mit Lücke ganz unten.
        for y in 0..11 {
            world.place_building(1, BuildingKind::Wall, 5, y).unwrap();
        }
        for _ in 0..20 * 30 {
            world.step(0.05);
            let u = &world.units[0];
            assert!(world.map.nav.is_open(u.pos.0 as i32, u.pos.1 as i32), "König läuft durch die Mauer");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 10.5).abs() < 1e-3 && (u.pos.1 - 1.5).abs() < 1e-3);
    }

    #[test]
    fn neue_luecke_wird_erst_nach_ruhe_genutzt() {
        let mut world = flat_world();
        let id = world.units[0].id;
        let wall: Vec<u32> = (0..11).map(|y| world.place_building(1, BuildingKind::Wall, 5, y).unwrap().id).collect();
        world.step(10.0); // Ruhe: alles neu geplant
        assert!(world.replan_pending.is_none());
        world.order_move(1, &[id], (10.5, 1.5));
        let before = world.units[0].path.clone();
        // Ein Stück Mauer weg: der König plant nicht sofort um, erst nach der Ruhezeit.
        world.demolish(1, wall[1]).unwrap();
        assert_eq!(world.units[0].path, before, "noch der alte Weg");
        assert!(world.replan_pending.is_some());
        let delay = world.economy.def.movement.replan_delay_seconds;
        let mut t = 0.0;
        while t < delay + 0.2 {
            world.step(0.05);
            t += 0.05;
        }
        assert!(world.replan_pending.is_none(), "nach der Ruhezeit neu geplant");
    }

    #[test]
    fn anhalten_bleibt_sofort_stehen() {
        let mut world = flat_world();
        let id = world.units[0].id;
        world.order_move(1, &[id], (15.5, 1.5));
        world.step(0.5);
        let at = world.units[0].pos;
        world.order_stop(2, &[id]);
        assert!(!world.units[0].path.is_empty(), "fremde Einheiten hält man nicht an");
        world.order_stop(1, &[id]);
        for _ in 0..20 {
            world.step(0.05);
        }
        let u = &world.units[0];
        assert!(u.path.is_empty() && u.waypoints.is_empty());
        assert!((u.pos.0 - at.0).abs() < 0.6 && (u.pos.1 - at.1).abs() < 0.6, "steht: {:?} statt {:?}", u.pos, at);
    }
}
