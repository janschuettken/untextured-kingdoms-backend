//! Gemeinsame Netzwerk-Nachrichten zwischen Server und Client.
//!
//! Die Rust-Typen hier sind die einzige Quelle der Wahrheit. Die passenden
//! TypeScript-Typen für den Client werden mit `cargo test -p protocol`
//! nach `protocol/bindings/` generiert. Im Frontend-Repo holt
//! `npm run sync-protocol` sie nach `src/protocol/`.
//!
//! Koordinaten: Positionen sind in Feld-Einheiten. Feld (x, y) belegt das
//! Quadrat von (x, y) bis (x + 1, y + 1), seine Mitte ist (x + 0.5, y + 0.5).
//! `y` zählt zeilenweise nach unten (wie in der Kartendatei).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Nachrichten vom Client an den Server.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ClientMsg {
    Hello { name: String },
    Ping { client_time: f64 },
    /// Bewegungsbefehl: Einheiten sollen zum Punkt (x, y) laufen.
    /// Der Server prüft Besitz und sucht selbst den Weg.
    /// Eine Gruppe läuft im Tempo ihrer langsamsten Einheit; `fast` (zweiter Befehl gleich
    /// hinterher, wie ein Doppelklick) lässt jede so schnell laufen, wie sie kann.
    MoveUnits {
        unit_ids: Vec<u32>,
        x: f32,
        y: f32,
        #[serde(default)]
        #[ts(optional)]
        fast: Option<bool>,
        /// Strg+Rechtsklick: als weiteren Wegpunkt hinten anhängen, statt den Weg zu ersetzen.
        /// Ein Befehl ohne `append` löscht alle alten Wegpunkte.
        #[serde(default)]
        #[ts(optional)]
        append: Option<bool>,
    },
    /// Angriffsbefehl (Rechtsklick auf einen Feind): die Einheiten laufen hin, verfolgen das
    /// Ziel und greifen an, bis es fällt. Wer dem Ziel nichts anhaben kann (Fernkämpfer gegen
    /// König oder Gebäude) oder ein Verbündeter ist, läuft nur hin.
    /// `force` (Knopf „Alles angreifen“): auch eigene, verbündete und neutrale Ziele.
    Attack {
        unit_ids: Vec<u32>,
        target: AttackTarget,
        #[serde(default)]
        #[ts(optional)]
        force: Option<bool>,
    },
    /// Einheiten, die reiten können, laufen zur nächsten eigenen Pferdezucht mit freiem
    /// Pferd und steigen dort auf.
    Mount { unit_ids: Vec<u32> },
    /// Reiter steigen ab; das Pferd läuft zurück in die nächste eigene Pferdezucht.
    Dismount { unit_ids: Vec<u32> },
    /// Sofort anhalten: Laufbefehl, weitere Wegpunkte und Angriffsbefehl verfallen.
    Stop { unit_ids: Vec<u32> },
    /// Gebäude bauen. (x, y) ist das Feld oben links der Grundfläche.
    /// Der Server prüft Untergrund, Platz und Besitz.
    /// `x`, `y` ist die linke obere Ecke der (gedrehten) Grundfläche.
    /// `rotation` in Vierteldrehungen: 0 = Eingang unten (Süden, +y), 1 = links (-x),
    /// 2 = oben (-y), 3 = rechts (+x). Bei 1 und 3 tauschen Breite und Tiefe.
    PlaceBuilding {
        kind: BuildingKind,
        x: u32,
        y: u32,
        #[serde(default)]
        rotation: u8,
        /// Gleich auf dieser Stufe bauen (0 oder 1 = erste Stufe). Kostet alle Stufen bis dahin.
        #[serde(default)]
        level: u8,
    },
    /// Brücke bauen: `x`, `y`, `w`, `h` ist die Reihe Wasserfelder unter dem Belag
    /// (eine Seite 1 Feld). Davor und dahinter muss Ufer sein.
    PlaceBridge { x: u32, y: u32, w: u32, h: u32 },
    /// Eigenes Gebäude abreißen. Ein Teil der Baukosten kommt zurück.
    DemolishBuilding { building_id: u32 },
    /// Eigenes Gebäude auf die nächste Stufe ausbauen.
    UpgradeBuilding { building_id: u32 },
    /// Beschädigtes Gebäude reparieren: kostet den fehlenden Anteil der Bau- und Ausbaukosten.
    RepairBuilding { building_id: u32 },
    /// Einstellungen eines eigenen Gebäudes ändern (was es herstellt usw.).
    ConfigureBuilding { building_id: u32, settings: BuildingSettings },
    /// Obstplantage aufgeben: ihre Bäume werden zu Wald, den man abholzen kann.
    ReleaseTrees { building_id: u32 },
    /// Sammelpunkt einer eigenen Kaserne setzen (`None` = wieder vor die Tür).
    SetRallyPoint { building_id: u32, point: Option<(f32, f32)> },
    /// Diese Einheit beobachten: der Server schickt ihren Weg (`UnitPath`). `None` = keine.
    WatchUnit { unit_id: Option<u32> },
    /// Laufkosten aller Felder anfordern (Heatmap). Antwort: `NavCosts`.
    RequestNavCosts,
    /// Zeitstrahl anfordern (Angriffe, Meilensteine, Ausscheiden). Antwort: `Timeline`.
    RequestTimeline,
    /// Baumeister bauen ein Belagerungsgerät: Baustelle mit der linken oberen Ecke bei (x, y),
    /// bei der Balliste ein Feld eines eigenen Turms. `backpack`: jeder nimmt gleich
    /// `army.builder_backpack` Baustoffe aus dem Vorrat mit.
    BuildSiege {
        unit_ids: Vec<u32>,
        kind: UnitKind,
        x: u32,
        y: u32,
        #[serde(default)]
        backpack: bool,
    },
    /// Diese eigenen Belagerungstürme fahren an das Gebäude (feindliche Mauer mit Wehrgang) und docken an.
    Dock { unit_ids: Vec<u32>, building_id: u32 },
    /// Die Bedienung dieser eigenen Belagerungsgeräte steigt ab und steht wieder frei.
    ReleaseCrew { unit_ids: Vec<u32> },
    /// In einer eigenen Kaserne `count` Soldaten dieser Art in Auftrag geben.
    /// Waren und Taler werden sofort bezahlt (alle, die bezahlbar sind).
    TrainUnits {
        building_id: u32,
        kind: UnitKind,
        count: u32,
        /// Auf alle eigenen Kasernen verteilen, die diese Einheit ausbilden können.
        #[serde(default)]
        spread: bool,
    },
    /// Soldaten aus der Warteschlange streichen; die Kosten kommen ganz zurück.
    /// Ohne `index` der zuletzt bestellte, sonst `count` (mindestens 1) ab `index`.
    CancelTraining {
        building_id: u32,
        #[serde(default)]
        #[ts(optional)]
        index: Option<u32>,
        #[serde(default)]
        #[ts(optional)]
        count: Option<u32>,
    },
    /// `count` Einträge ab `index` in der Warteschlange nach vorn ziehen: gleich
    /// hinter den, der gerade ausgebildet wird (dessen Fortschritt bleibt).
    PrioritizeTraining { building_id: u32, index: u32, count: u32 },
    /// Am Markt handeln: `sell` = verkaufen, sonst kaufen. Bezahlt wird in Talern.
    Trade { good: Good, amount: u32, sell: bool },
    /// Automatisch handeln: unter `buy_below` kaufen, über `sell_above` verkaufen (0 = aus).
    SetAutoTrade { good: Good, buy_below: u32, sell_above: u32 },
    /// Grenzen einer Ware für neue Betriebe merken (`None` = unverändert, 0 = aus).
    SetGoodLimit {
        good: Good,
        #[serde(default)]
        stop_at: Option<u32>,
        #[serde(default)]
        keep_in_stock: Option<u32>,
    },
    /// Rationen und Steuersatz für die eigenen Bewohner (Stufen aus `population.rations`
    /// und `market.tax_levels`; `None` = unverändert).
    SetPeoplePolicy {
        #[serde(default)]
        ration: Option<u32>,
        #[serde(default)]
        tax: Option<u32>,
    },
    /// Als Admin anmelden (Token aus der Server-Umgebung `ADMIN_TOKEN`).
    AdminLogin { token: String },
    /// Nur Admin: sich selbst Waren ins Lager legen.
    AdminGive {
        goods: Vec<GoodAmount>,
        #[serde(default)]
        taler: u32,
    },
    /// Nur Admin: Spieltempo für alle in Prozent (100 = normal, 25 bis 800, 0 = Pause).
    AdminGameSpeed { percent: u32 },
    /// Pause drücken (`true`) oder die eigene Pause wieder aufheben. Das Spiel steht,
    /// solange mindestens ein Spieler Pause gedrückt hat.
    SetPause { paused: bool },
    /// Sichtbereich des Clients in Feldern (Rechteck, Ecken eingeschlossen).
    /// Der Server schickt Einheiten nur für diesen Bereich (plus Rand) und
    /// immer die eigenen. Ohne diese Nachricht sieht der Client alles.
    View { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// Eigenes Gebäude an eine andere Stelle versetzen (kostet `move_cost_percent` der Baukosten).
    MoveBuilding { building_id: u32, x: u32, y: u32, #[serde(default)] rotation: u8 },
    /// Waren (und Taler) an einen Verbündeten schicken, ohne Markt. Wie sie ankommen,
    /// legt `TradeMode` des Spiels fest (sofort oder mit Handelskarren).
    SendGoods {
        to: u32,
        goods: Vec<GoodAmount>,
        #[serde(default)]
        taler: u32,
    },
}

/// Nachrichten vom Server an den Client.
// `welcome` ist groß, kommt aber nur einmal pro Verbindung.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerMsg {
    Welcome {
        player_id: u32,
        tick_rate: u32,
        map: MapRef,
        /// Waren, Gebäude, Stufen und Rezepte aus `server/data/economy.json`.
        /// So pflegt der Client nichts doppelt und kann die Produktionsketten anzeigen.
        economy: EconomyDef,
        /// Eigener Lagerbestand zu Beginn.
        stock: StockState,
        /// Alle Gebäude, die schon stehen. Danach kommen nur noch Änderungen.
        buildings: Vec<BuildingState>,
        /// Wie Fernkämpfer schießen (für die Reichweitenanzeige).
        #[serde(default)]
        shot_mode: ShotMode,
        /// Spielregeln (z. B. ob Munition verbraucht wird).
        #[serde(default)]
        rules: GameRules,
    },
    Tick {
        /// JSON-Zahl; reicht bei 20 Ticks/s für Millionen Jahre.
        #[ts(type = "number")]
        tick: u64,
        players_online: u32,
        /// Gespielte Spielzeit in Sekunden (ohne Pausen, mit Spieltempo); bleibt am Spielende stehen.
        #[serde(default)]
        game_seconds: f64,
        /// Einheiten, die dieser Client sehen soll und die neu im Bild sind
        /// oder sich verändert haben. Alle anderen sind unverändert.
        units: Vec<UnitState>,
        /// Einheiten, die verschwunden oder aus dem Bild gelaufen sind.
        gone: Vec<u32>,
    },
    Pong {
        client_time: f64,
    },
    BuildingPlaced {
        building: BuildingState,
    },
    BuildingRemoved {
        building_id: u32,
    },
    /// Ein Gebäude hat sich geändert (z. B. ausgebaut).
    BuildingChanged {
        building: BuildingState,
    },
    /// Rohstoff auf einem Feld hat sich geändert (z. B. Wald abgeholzt).
    ResourceChanged {
        x: u32,
        y: u32,
        resource: ResourceKind,
    },
    /// Eigener Lagerbestand hat sich geändert (geht nur an diesen Spieler).
    Stock {
        stock: StockState,
    },
    /// Ein Befehl dieses Clients wurde abgelehnt (z. B. kein Platz zum Bauen).
    Rejected {
        text: Text,
    },
    /// Laufbefehl abgelehnt: keine der Einheiten kommt zu diesem Punkt.
    MoveUnreachable {
        x: f32,
        y: f32,
    },
    /// Zeitstrahl des Spiels (Antwort auf `RequestTimeline`), älteste zuerst. Solange das
    /// Spiel läuft, nur was den Spieler oder seine Verbündeten betrifft; nach Spielende
    /// oder nach dem eigenen Ausscheiden alles.
    Timeline {
        entries: Vec<TimelineEntry>,
        /// Aktuelle Spielzeit in Sekunden.
        now: f64,
    },
    /// Laufkosten je Feld (Antwort auf `RequestNavCosts`), zeilenweise, ein Zeichen je Feld:
    /// `' '` = nicht begehbar, sonst Tempo in Prozent = (Zeichencode - 48) * 5.
    NavCosts {
        width: u32,
        height: u32,
        cells: String,
    },
    /// Antwort auf `admin_login`: angenommen oder nicht.
    AdminStatus {
        ok: bool,
    },
    /// Spieltempo in Prozent (100 = normal). Kommt beim Verbinden und nach jeder Änderung.
    GameSpeed {
        percent: u32,
    },
    /// Wer gerade Pause gedrückt hat (Spieler-Nummern). Leer = das Spiel läuft.
    /// Kommt beim Verbinden und nach jeder Änderung.
    Pauses {
        players: Vec<u32>,
    },
    /// Die Wirtschaftsdaten wurden im Balancing-Editor geändert und gelten ab sofort.
    EconomyChanged {
        economy: EconomyDef,
    },
    /// Spielstand wurde geladen oder neu gestartet: Seite neu laden.
    Reload,
    /// Dieses Spiel gibt es nicht (mehr), oder man ist dort kein Mitspieler.
    /// Der Client geht zurück in die Lobby; die Verbindung wird geschlossen.
    NotInGame,
    /// Derselbe Spieler hat das Spiel in einem anderen Fenster geöffnet.
    /// Dieses Fenster verbindet nicht neu.
    OpenedElsewhere,
    /// Geplanter Weg der beobachteten Einheit (siehe `WatchUnit`), nächster Wegpunkt zuerst.
    /// Leer = steht. Kommt nur, wenn sich der Weg ändert.
    UnitPath {
        unit_id: u32,
        path: Vec<(f32, f32)>,
    },
    /// Ist die eigene Burg geschlossen? Kommt beim Verbinden und wenn es sich ändert.
    Castle {
        status: CastleStatus,
    },
    /// Was die beobachtete eigene Einheit tut, wohin und mit welcher Ladung.
    /// Kommt nur, wenn sich etwas ändert.
    UnitInfo {
        info: UnitInfo,
    },
    /// Mitspieler, Bündnisse und Handelsart. Kommt nach `welcome`.
    Diplomacy {
        players: Vec<PlayerInfo>,
        trade_mode: TradeMode,
    },
    /// Waren zwischen Verbündeten: losgeschickt (`arrived` = false, nur bei Karren)
    /// oder angekommen. Geht nur an Absender und Empfänger.
    Shipment {
        from: u32,
        to: u32,
        goods: Vec<GoodAmount>,
        taler: u32,
        arrived: bool,
        /// Wie viele Handelskarren fahren (0 = sofort angekommen).
        carts: u32,
        /// Beute: `from` ist ausgeschieden, `to` hat seinen König getötet.
        #[serde(default)]
        loot: bool,
    },
    /// Der König dieses Spielers ist tot: er scheidet aus, alles von ihm ist verschwunden.
    /// Geht an alle; ein ausgeschiedener Spieler bekommt es beim Verbinden noch einmal.
    PlayerDefeated {
        player: u32,
        /// Wessen Einheit den letzten Schlag geführt hat.
        by: Option<u32>,
    },
    /// Diese Einheiten sind im Kampf gefallen (der Client lässt sie umfallen und versinken).
    UnitsDied {
        unit_ids: Vec<u32>,
    },
    /// Schüsse der Fernkämpfer in diesem Takt (für fliegende Pfeile und Bolzen im Client).
    Shots {
        shots: Vec<Shot>,
    },
    /// Behandlungen der Heiler in diesem Takt (für das grüne Leuchten im Client).
    Heals {
        heals: Vec<Heal>,
    },
    /// Warnung für einen Spieler (geht nur an ihn): Feinde nahe der Burg oder ein Gebäude
    /// wird angegriffen. `x`/`y` = wo (für den Sprung der Kamera).
    Alert {
        player: u32,
        alert: AlertKind,
        x: f32,
        y: f32,
        /// Bei `building_attacked`: welches Gebäude.
        building: Option<BuildingKind>,
    },
    /// Nur noch ein Spieler (oder ein Bündnis) hat seinen König: das Spiel ist entschieden.
    GameOver {
        winners: Vec<u32>,
    },
}

/// Ein Eintrag im Zeitstrahl. Angriffe sind Vorfälle: Geht es innerhalb von 30 s weiter,
/// wird derselbe Eintrag bis `until` verlängert statt ein neuer angelegt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TimelineEntry {
    /// Spielzeit (Sekunden) des Ereignisses bzw. des Beginns des Vorfalls.
    pub at: f64,
    /// Spielzeit des letzten Treffers im Vorfall (sonst gleich `at`).
    pub until: f64,
    pub event: TimelineEvent,
}

/// Was im Zeitstrahl steht. `player` = der Betroffene, `by` = wer angreift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TimelineEvent {
    /// Feindliche Kämpfer von `by` nahe der Burg von `player` (wie die Warnung).
    EnemyNear { player: u32, by: u32 },
    /// `by` beschädigt Gebäude von `player` (die Arten, die im Vorfall getroffen wurden).
    BuildingAttacked { player: u32, by: u32, buildings: Vec<BuildingKind> },
    /// `by` greift Einheiten von `player` an.
    UnitsAttacked { player: u32, by: u32 },
    /// Meilenstein eines Spielers (mit der Einheit, die ihn ausgelöst hat).
    Milestone { player: u32, milestone: Milestone, unit: Option<UnitKind> },
    /// König gefallen, `by` hat den letzten Schlag geführt.
    PlayerDefeated { player: u32, by: Option<u32> },
    /// Spiel entschieden.
    GameOver { winners: Vec<u32> },
}

/// Meilensteine im Zeitstrahl, je Spieler höchstens einmal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Milestone {
    /// Erster Soldat (Speerträger, Bogenschütze, …).
    FirstSoldier,
    /// Erste mittlere Einheit (Armbrustschütze, Schwertkämpfer mit Rüstung).
    FirstMedium,
    /// Erste schwere Einheit (Ritter).
    FirstHeavy,
    /// Erstes Belagerungsgerät.
    FirstSiege,
    /// Burg zum ersten Mal geschlossen (kein Weg von außen zum Bergfried).
    CastleClosed,
}

/// Art einer Warnung (siehe `ServerMsg::Alert`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AlertKind {
    /// Feindliche Kämpfer im Umkreis `army.alert_radius` um den eigenen Bergfried.
    EnemyNear,
    /// Ein eigenes Gebäude nimmt Schaden.
    BuildingAttacked,
}

/// Ein Spieler im laufenden Spiel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlayerInfo {
    pub id: u32,
    pub name: String,
    /// Bündnis: Spieler mit derselben Zahl (ab 1) sind verbündet, 0 = allein.
    pub team: u8,
    pub computer: bool,
}

/// Wie Waren zwischen Verbündeten ankommen (Einstellung des Spiels in der Lobby).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TradeMode {
    /// Sofort im Lager des Verbündeten.
    #[default]
    Instant,
    /// Handelskarren fahren vom eigenen Lager zu seinem und verschwinden dort.
    Carts,
}

/// Wann Fernkämpfer treffen können (Einstellung des Spiels in der Lobby).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ShotMode {
    /// Wurfbahn mit dem Tempo für die größte Reichweite (flach oder steil): stößt sie an
    /// Gelände, Gebäude oder Mauer, wird nicht geschossen. Über Mauern hinweg geht es.
    #[default]
    Ballistic,
    /// Der Schütze muss das Ziel sehen: eine gerade, freie Linie.
    LineOfSight,
}

/// Spielregeln, die der Gastgeber in der Lobby wählt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GameRules {
    /// Bogen- und Armbrustschützen verbrauchen Pfeile (sonst unbegrenzt).
    #[serde(default = "yes")]
    pub arrows: bool,
    /// Belagerungsgeräte verbrauchen Munition (Steine, Bretter; sonst unbegrenzt).
    #[serde(default = "yes")]
    pub siege_ammo: bool,
}

impl Default for GameRules {
    fn default() -> Self {
        GameRules { arrows: true, siege_ammo: true }
    }
}

/// Nachrichten vom Client an die Lobby (WebSocket `/lobby`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum LobbyClientMsg {
    /// Als Erstes: wer man ist. `token` erzeugt der Browser einmal und
    /// speichert ihn (geheim, nur der Server kennt ihn sonst).
    Hello { token: String, name: String },
    SetName { name: String },
    Ping { client_time: f64 },
    /// Neues Spiel (noch in der Lobby) auf einer Karte aus dem Kartenordner.
    CreateGame { map: String },
    /// Einem wartenden Spiel beitreten. Laufende Spiele nehmen niemanden auf.
    JoinGame { game_id: u32 },
    /// Ein eigenes Spiel verlassen: das laufende mit dieser Id, ohne Id das wartende.
    LeaveGame {
        #[serde(default)]
        #[ts(optional)]
        game_id: Option<u32>,
    },
    /// Startplatz wechseln (nur auf einen freien).
    TakeSlot { slot: u32 },
    /// Nur Gastgeber, nur vor dem Start: wer auf Startplatz `from` sitzt, kommt auf `to`
    /// (auf der Kartenvorschau gezogen). Ist `to` besetzt, tauschen beide.
    MoveSlot { from: u32, to: u32 },
    /// Nur Gastgeber, nur vor dem Start: andere Karte.
    SetMap { map: String },
    /// Nur Gastgeber: Spiel starten.
    StartGame,
    /// Nur Gastgeber, nur vor dem Start: einen Computergegner auf diesen freien Startplatz setzen.
    AddComputer {
        slot: u32,
        #[serde(default)]
        level: AiLevel,
        /// Charakter (Id aus `ai_personalities.json`), leer = der erste.
        #[serde(default)]
        #[ts(optional)]
        personality: Option<String>,
    },
    /// Nur Gastgeber, nur vor dem Start: den Computergegner von diesem Startplatz nehmen.
    RemoveComputer { slot: u32 },
    /// Nur vor dem Start: Bündnis eines Startplatzes (0 = keins). Den eigenen darf
    /// jeder setzen, die der Computergegner nur der Gastgeber.
    SetTeam { slot: u32, team: u8 },
    /// Nur Gastgeber, nur vor dem Start: Regler eines Computergegners (0 bis 100) für dieses
    /// Spiel. Leer = wie sein Charakter.
    SetComputerTraits {
        slot: u32,
        #[serde(default)]
        #[ts(optional)]
        daring: Option<u8>,
        #[serde(default)]
        #[ts(optional)]
        helpfulness: Option<u8>,
    },
    /// Nur Gastgeber, nur vor dem Start: Startvorteil eines Computergegners in Prozent
    /// (0 bis 200): so viel mehr Startwaren und Taler, je 25 % ein Soldat dazu.
    SetComputerBonus { slot: u32, bonus: u8 },
    /// Nur Gastgeber, nur vor dem Start: wie Waren zwischen Verbündeten ankommen.
    SetTradeMode { mode: TradeMode },
    /// Nur Gastgeber, nur vor dem Start: wann Fernkämpfer treffen (Wurfbahn oder Sichtlinie).
    SetShotMode { mode: ShotMode },
    /// Nur Gastgeber, nur vor dem Start: Spielregeln (Munition).
    SetRules { rules: GameRules },
    /// Gastgeber oder Admin, jederzeit: Namen eines Spiels setzen (leer = Standardname).
    RenameGame { game_id: u32, title: String },
    /// Beendete Spiele löschen: Admins alle, sonst die, in denen man mitspielt.
    CleanupGames,
}

/// Stärke eines Computergegners. Was jede Stufe tut, steht im Server (`server/src/ai/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AiLevel {
    /// Baut langsam, kleine Armee, Palisade.
    #[default]
    Easy,
    /// Baut schneller, mehr Soldaten, Steinmauer.
    Normal,
    /// Baut am schnellsten, große Armee, Steinmauer mit Torhaus und Ecktürmen,
    /// gepflasterte Wege, handelt am Markt.
    Hard,
}

/// Nachrichten der Lobby an den Client.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum LobbyServerMsg {
    /// Antwort auf `hello`: die eigene öffentliche Id, die Karten zur Auswahl und
    /// die Version des Servers (Commit und Datum).
    Welcome { you: String, maps: Vec<String>, server_version: String },
    /// Kompletter Stand, kommt bei jeder Änderung neu. `current` ist das wartende
    /// Spiel, in dem man sitzt; laufende eigene Spiele stehen in `games` (`mine`).
    State { games: Vec<GameSummary>, current: Option<LobbyGame> },
    Pong { client_time: f64 },
    Rejected { text: Text },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GamePhase {
    /// Wartet in der Lobby, man kann beitreten.
    Waiting,
    /// Läuft: nur die eigenen Mitspieler kommen (wieder) hinein.
    Running,
}

/// Ein Spiel in der Übersicht.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GameSummary {
    pub id: u32,
    /// Vom Gastgeber gesetzter Name (leer: „Spiel von …“).
    #[serde(default)]
    pub title: String,
    pub map: String,
    /// Kartengröße in Feldern.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    pub phase: GamePhase,
    pub host_name: String,
    /// Öffentliche Id des Gastgebers (wer umbenennen darf).
    #[serde(default)]
    pub host: String,
    pub players: u32,
    pub max_players: u32,
    /// Man spielt selbst mit (laufende Spiele: man kann weiterspielen).
    pub mine: bool,
    /// Läuft, steht aber still: kein Mensch im Spiel (oder Tempo 0).
    pub paused: bool,
    /// Wer mitspielt (für den Admin: „Spielen als“ hinter jedem Spieler).
    pub slots: Vec<LobbySlot>,
    /// Zuletzt gespielt (Unix-Sekunden; wartende Spiele: angelegt). Die Liste ist danach sortiert.
    #[serde(default)]
    #[ts(type = "number")]
    pub last_played: u64,
    /// Entschieden (ein König oder Bündnis ist übrig): kann aufgeräumt werden.
    #[serde(default)]
    pub finished: bool,
}

/// Das Spiel, in dem man selbst ist, mit allen Startplätzen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LobbyGame {
    pub id: u32,
    /// Vom Gastgeber gesetzter Name (leer: „Spiel von …“).
    #[serde(default)]
    pub title: String,
    pub map: String,
    pub width: u32,
    pub height: u32,
    pub phase: GamePhase,
    /// Öffentliche Id des Gastgebers.
    pub host: String,
    /// Ein Eintrag je Startplatz der Karte, in derselben Reihenfolge.
    pub slots: Vec<LobbySlot>,
    /// Handel unter Verbündeten: sofort oder mit Karren.
    #[serde(default)]
    pub trade_mode: TradeMode,
    /// Fernkampf: Wurfbahn oder Sichtlinie.
    #[serde(default)]
    pub shot_mode: ShotMode,
    #[serde(default)]
    pub rules: GameRules,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LobbySlot {
    /// Startfeld auf der Karte.
    pub x: u32,
    pub y: u32,
    /// Spieler-Nummer im Spiel (bestimmt die Farbe).
    pub player_id: u32,
    pub member: Option<LobbyMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LobbyMember {
    /// Öffentliche Id (nicht der geheime Token).
    pub id: String,
    pub name: String,
    /// Gerade verbunden (Lobby oder Spiel). Computergegner sind immer da.
    pub online: bool,
    /// Gesetzt bei einem Computergegner: seine Stärke.
    #[serde(default)]
    pub computer: Option<AiLevel>,
    /// Computergegner: sein Charakter (Id aus `ai_personalities.json`).
    #[serde(default)]
    pub personality: Option<String>,
    /// Bündnis (gleiche Zahl ab 1 = verbündet), 0 = keins.
    #[serde(default)]
    pub team: u8,
    /// Computergegner: eigener Wert für „Angriff trotz Unterlegenheit“ (sonst der des Charakters).
    #[serde(default)]
    pub daring: Option<u8>,
    /// Computergegner: eigener Wert für „Hilfsbereitschaft“ (sonst der des Charakters).
    #[serde(default)]
    pub helpfulness: Option<u8>,
    /// Laufendes Spiel: sein König ist gefallen, er ist ausgeschieden.
    #[serde(default)]
    pub defeated: bool,
    /// Computergegner: Startvorteil in Prozent (mehr Waren und Taler, Soldaten dazu).
    #[serde(default)]
    pub bonus: u8,
}

/// Kleine Kartenvorschau für die Lobby (`GET /api/games/{id}/preview`).
/// Jede Zelle fasst `step` x `step` Felder zusammen (Mitte gezählt).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GamePreview {
    pub width: u32,
    pub height: u32,
    pub step: u32,
    pub cols: u32,
    pub rows: u32,
    /// `cols * rows` Zeichen, Zeile für Zeile: `~` Wasser, `W` Wald, `S` Stein,
    /// `E` Eisen, `A`/`F` Acker bzw. fruchtbar, sonst die Höhenstufe `0` bis `9`.
    /// Ressourcen gelten im laufenden Spiel mit dem aktuellen Stand.
    pub cells: String,
    /// Laufendes Spiel, nur für Mitspieler und Admins: Gebäude (in Feldern).
    pub buildings: Vec<PreviewBuilding>,
    /// Laufendes Spiel: ausgeschiedene Spieler.
    pub defeated: Vec<u32>,
    /// Laufendes Spiel, sobald entschieden: die Gewinner.
    pub winners: Option<Vec<u32>>,
}

/// Ein Gebäude in der Kartenvorschau.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PreviewBuilding {
    pub owner: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// `building`, `wall` (Befestigung), `road` (Weg, Brücke) oder `keep`.
    pub look: PreviewLook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PreviewLook {
    Building,
    Wall,
    Road,
    Keep,
}

/// Ob ein Gegner zu Fuß bis an den eigenen Bergfried kommt (eigene Tore gelten als zu,
/// außer sie stehen auf „offen“).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CastleStatus {
    /// Hat überhaupt Mauern, Tore oder Türme.
    pub walled: bool,
    /// Kein Weg von einem fremden Startplatz zum Bergfried: man muss erst durch die Mauer.
    pub closed: bool,
    /// Offen: hier geht der Weg hinein (Feld an der eigenen Befestigung), falls es eins gibt.
    pub gap: Option<(u32, u32)>,
}

/// Charaktere der Computergegner (Datei `server/data/ai_personalities.json`, Editor `/ai.html`).
/// Die Stärke (`AiLevel`) bestimmt, wie gut er spielt; der Charakter, was er lieber baut und
/// ausbildet und wie gern er angreift. Beides wird getrennt gewählt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AiPersonalities {
    pub personalities: Vec<AiPersonality>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AiPersonality {
    /// Kurz, nur Kleinbuchstaben, Ziffern und `_` (steht im Spielstand).
    pub id: String,
    /// Name je Sprache (`de`, `en`, ...).
    #[serde(default)]
    pub name: std::collections::BTreeMap<String, String>,
    /// Beschreibung je Sprache.
    #[serde(default)]
    pub description: std::collections::BTreeMap<String, String>,
    /// Angriffslust in Prozent: 100 = wie die Stufe, 0 = greift nie an, 200 = früher, öfter,
    /// auch gegen stärkere Gegner.
    #[serde(default = "percent_100")]
    pub aggression: u32,
    /// Armeegröße in Prozent der Stufe.
    #[serde(default = "percent_100")]
    pub army: u32,
    /// Angriff trotz Unterlegenheit, 0 bis 100: 0 = nur, wenn er sich stärker glaubt;
    /// 100 = ständig, sobald eine Welle bereit ist. Dazwischen greift er umso eher an, je
    /// länger er schon wartet.
    #[serde(default = "daring_default")]
    pub daring: u32,
    /// Hilfsbereitschaft gegenüber Verbündeten, 0 bis 100: 0 = hilft nie; sonst schickt er
    /// Truppen (höchstens diesen Anteil seiner freien Soldaten) und Waren, wenn ein
    /// Verbündeter angegriffen wird.
    #[serde(default = "helpfulness_default")]
    pub helpfulness: u32,
    /// Gebäude in Prozent: 100 = wie die Stufe, 0 = baut es nie, 50 = halb so viele,
    /// 200 = doppelt so viele. Steht es nicht in der Bauliste der Stufe, baut er ab 200 eins
    /// (300 zwei, ...). Fehlende Gebäude gelten als 100.
    #[serde(default)]
    pub buildings: std::collections::BTreeMap<BuildingKind, u32>,
    /// Grundriss der Burg.
    #[serde(default)]
    pub castle: CastleLayout,
    /// Soldaten in Prozent: wie oft er sie im Verhältnis ausbildet. 0 = nie. Soldaten, die die
    /// Stufe nicht ausbildet, nimmt er ab 1 dazu (und baut, was sie brauchen).
    #[serde(default)]
    pub units: std::collections::BTreeMap<UnitKind, u32>,
}

/// Wie ein Computergegner seine Burg anlegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CastleLayout {
    /// Wie die Stufe es vorsieht (leicht: eckig, normal: achteckig, schwer: Engstelle).
    #[default]
    Level,
    /// Rechteckiger Ring mit einem Tor je Seite.
    Square,
    /// Ring mit abgeschrägten Ecken (mehr Platz, Türme auf den Schrägen).
    Octagon,
    /// Mauer nur an Engstellen, die Wasser, Wald, Fels oder Hänge lassen; sonst eckig.
    Narrows,
}

fn percent_100() -> u32 {
    100
}

fn daring_default() -> u32 {
    30
}

fn helpfulness_default() -> u32 {
    50
}

/// Art eines Feldes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TileKind {
    /// Flacher Boden auf einer Höhenstufe.
    Ground,
    /// Begehbare Schräge zwischen zwei Höhenstufen.
    Ramp,
    /// Wasser (Fluss, Burggraben): nicht begehbar.
    Water,
}

/// Welche Karte gespielt wird. Die Karte selbst (Höhen, Ressourcen) holt der
/// Client als Text von `GET /api/games/{id}/map` (Format wie `server/maps/*.txt`,
/// mit dem aktuellen Stand der Ressourcen) und rechnet Eckhöhen selbst aus.
/// Änderungen danach kommen als [`ServerMsg::ResourceChanged`].
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapRef {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

/// Ressource auf einem Feld. In der Kartendatei die Ebene `[resources]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ResourceKind {
    /// `.` nichts
    None,
    /// `W` Wald: blockiert Einheiten, bis er abgeholzt ist.
    Forest,
    /// `S` Steinbruch (Felsen, blockiert Einheiten)
    Stone,
    /// `E` Eisenvorkommen (Felsen, blockiert Einheiten)
    Iron,
    /// `A` Ackerboden
    Farmland,
    /// `F` besonders fruchtbarer Boden
    Fertile,
}

/// Ein Problem in einer Karte, optional mit dem betroffenen Feld.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapIssue {
    pub x: Option<u32>,
    pub y: Option<u32>,
    /// Übersetzbarer Text, Schlüssel `map.*`.
    pub text: Text,
}

/// Ergebnis der Kartenprüfung (für den Karteneditor, `POST /api/maps/check`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapCheck {
    /// Größe der Karte, 0 wenn sich das Format gar nicht lesen ließ.
    pub width: u32,
    pub height: u32,
    /// Startfelder (x, y).
    pub spawns: Vec<(u32, u32)>,
    /// Fehler: so lässt sich die Karte nicht spielen oder speichern.
    pub errors: Vec<MapIssue>,
    /// Hinweise: spielbar, aber wahrscheinlich ungewollt.
    pub warnings: Vec<MapIssue>,
}

/// Ein Schuss: von wo nach wo (Feld-Koordinaten), wer schoss und womit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Shot {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub from_wall: bool,
    pub to_wall: bool,
    pub kind: UnitKind,
    /// Höhe (Welt) am Start und am Ziel.
    pub from_h: f32,
    pub to_h: f32,
    /// Wurfbahn: Steigung beim Abschuss (tan) und Reichweite des Schützen. `None` = gerade Linie.
    pub arc: Option<(f32, f32)>,
    /// Flugzeit bis zum Aufschlag (Sekunden); erst dann zählt der Treffer auf dem Server.
    #[serde(default)]
    pub seconds: f32,
}

/// Ein Heiler behandelt einen Verwundeten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Heal {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub from_wall: bool,
    pub to_wall: bool,
    /// Wer geheilt wurde.
    pub unit_id: u32,
}

/// Ziel eines Angriffsbefehls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum AttackTarget {
    Unit { id: u32 },
    /// Gebäude, Mauern und Tore: nur Nahkämpfer reißen sie ein.
    Building { id: u32 },
}

/// Art einer Einheit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UnitKind {
    King,
    /// Träger: trägt wenig, wohnt im Bergfried oder Vorratslager.
    Carrier,
    /// Karren aus dem Wagenschuppen: fährt viel auf einmal.
    Cart,
    /// Arbeiter eines Abbaubetriebs: holt Holz, Stein oder Erz und trägt es zurück.
    Worker,
    /// Soldaten aus der Kaserne (Werte in `economy.json` unter `units`).
    /// Speerträger: billig, Speer aus dem Speermacher.
    Spearman,
    /// Bogenschütze mit Langbogen.
    Archer,
    /// Armbrustschütze: langsamer, trifft härter.
    Crossbowman,
    /// Einfacher Schwertkämpfer: Schwert und nur leichte Rüstung.
    LightSwordsman,
    /// Schwertkämpfer mit Rüstung.
    Swordsman,
    /// Ritter zu Pferd: schnell und stark, teuer.
    Knight,
    /// Handelskarren: bringt Waren zu einem Verbündeten und verschwindet dort.
    TradeCart,
    /// Heiler aus der Apotheke: kämpft nicht, heilt verwundete eigene und verbündete Einheiten in der Nähe.
    Healer,
    /// Baumeister aus der Belagerungswerkstatt: baut Belagerungsgeräte im Feld und bedient sie.
    Builder,
    /// Katapult: rollt langsam, wirft Steine auf Gebäude und Einheiten.
    Catapult,
    /// Tribok: steht fest, wirft weit und schwer, nur auf Gebäude.
    Trebuchet,
    /// Balliste: steht auf einem eigenen Turm und schießt schwere Bolzen auf Einheiten.
    Ballista,
    /// Belagerungsturm: rollt an eine feindliche Mauer, dockt an, Truppen steigen über ihn hinauf.
    SiegeTower,
}

impl UnitKind {
    /// Meilensteine, die diese Einheit beim Ausbilden bzw. Bauen auslösen kann.
    pub fn milestones(self) -> &'static [Milestone] {
        use UnitKind::*;
        match self {
            Spearman | Archer | LightSwordsman => &[Milestone::FirstSoldier],
            Crossbowman | Swordsman => &[Milestone::FirstSoldier, Milestone::FirstMedium],
            Knight => &[Milestone::FirstSoldier, Milestone::FirstHeavy],
            Catapult | Trebuchet | Ballista | SiegeTower => &[Milestone::FirstSiege],
            _ => &[],
        }
    }

    /// Soldaten: werden in der Kaserne ausgebildet und kosten Sold.
    pub fn is_soldier(self) -> bool {
        matches!(self, UnitKind::Spearman | UnitKind::Archer | UnitKind::Crossbowman | UnitKind::LightSwordsman | UnitKind::Swordsman | UnitKind::Knight | UnitKind::Healer | UnitKind::Builder)
    }

    /// Belagerungsgeräte: entstehen auf einer Baustelle und brauchen Baumeister als Bedienung.
    pub fn is_siege(self) -> bool {
        matches!(self, UnitKind::Catapult | UnitKind::Trebuchet | UnitKind::Ballista | UnitKind::SiegeTower)
    }

    /// Zivilisten: Arbeiter, Träger und Karren. Sie kämpfen nicht, können aber angegriffen werden.
    pub fn is_civilian(self) -> bool {
        matches!(self, UnitKind::Carrier | UnitKind::Cart | UnitKind::Worker | UnitKind::TradeCart)
    }
}

/// Was eine Einheit gerade tut (für die Anzeige beim Anklicken).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UnitActivity {
    /// Steht und hat nichts zu tun.
    Idle,
    /// Läuft zu einem Punkt (Befehl oder auf dem Rückweg).
    Walking,
    /// Steht auf dem Wehrgang.
    OnWall,
    /// Arbeiter: unterwegs zum Rohstoff-Feld.
    ToResource,
    /// Arbeiter: baut am Feld ab.
    Gathering,
    /// Arbeiter: arbeitet in seinem Gebäude.
    Working,
    /// Arbeiter: bringt die Ware zum eigenen Betrieb.
    BringingHome,
    /// Bringt Ware in ein Lager.
    Delivering,
    /// Holt Ware für den eigenen Betrieb.
    Fetching,
    /// Träger/Karren: unterwegs zum Aufladen.
    PickingUp,
    /// Träger/Karren: bringt die Ladung hin.
    Unloading,
    /// Träger/Karren: zurück zum Heimatgebäude.
    Returning,
    /// Läuft zur Pferdezucht, um aufzusteigen.
    ToHorse,
    /// Vom gefallenen Pferd gestürzt: liegt am Boden und kann eine Weile nicht kämpfen.
    Fallen,
}

/// Genaueres zu einer beobachteten eigenen Einheit (siehe `WatchUnit`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnitInfo {
    pub unit_id: u32,
    pub activity: UnitActivity,
    /// Gebäude, zu dem die Einheit gehört (Arbeiter, Träger, Karren).
    pub home: Option<u32>,
    /// Gebäude, zu dem sie gerade unterwegs ist.
    pub target_building: Option<u32>,
    /// Endpunkt ihres Weges (Feld-Koordinaten).
    pub target: Option<(f32, f32)>,
    /// Ware, um die es beim Auftrag geht (auch wenn noch nicht geladen).
    pub job_good: Option<Good>,
    /// Was sie gerade trägt.
    pub carrying: Option<GoodAmount>,
}

/// Zustand einer Einheit in einem Tick.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnitState {
    pub id: u32,
    pub owner: u32,
    pub kind: UnitKind,
    pub x: f32,
    pub y: f32,
    pub moving: bool,
    /// Was ein Träger oder Karren gerade transportiert.
    #[serde(default)]
    pub carrying: Option<Good>,
    /// Läuft oben auf der Mauer (Wehrgang) statt am Boden.
    #[serde(default)]
    pub on_wall: bool,
    /// Arbeiter ist in seinem Gebäude (steht dort und arbeitet): außen nicht zu sehen.
    #[serde(default)]
    pub inside: bool,
    /// Lebenspunkte (Höchstwert: `hp` der Einheit in `economy.units`). 0 = hat keine (Träger, Arbeiter).
    #[serde(default)]
    pub hp: u32,
    /// Sitzt auf einem Pferd.
    #[serde(default)]
    pub mounted: bool,
    /// Lebenspunkte des Pferdes (Höchstwert: `army.horse_hp`), 0 ohne Pferd.
    #[serde(default)]
    pub horse_hp: u32,
    /// Sekunden, die die Einheit nach einem Sturz noch am Boden liegt (0 = steht).
    #[serde(default)]
    pub down: f32,
    /// Wen die Einheit gerade angreift (Angriffsbefehl oder zuletzt selbst gewähltes Ziel).
    #[serde(default)]
    pub attack_target: Option<AttackTarget>,
    /// Zählt jeden Schlag und Schuss: ändert er sich, holt die Figur aus (Animation).
    #[serde(default)]
    pub strikes: u32,
    /// Belagerungsgerät: so viele Baumeister bedienen es gerade (nötig: `siege.crew`).
    #[serde(default)]
    pub crew: u32,
    /// Baumeister: bedient gerade dieses Gerät (fährt mit, wird nicht als eigene Einheit gezeigt).
    #[serde(default)]
    pub crew_of: Option<u32>,
    /// Belagerungsgerät mit begrenztem Vorrat: so viele Schüsse sind noch da (sonst None).
    #[serde(default)]
    pub shots_left: Option<u32>,
    /// Belagerungsturm: liegt an diesem Gebäude (Mauer) an.
    #[serde(default)]
    pub docked_at: Option<u32>,
}

/// Ein Text, den der Client übersetzt: Schlüssel plus Platzhalter.
/// Der Server schickt nie fertige Sätze, so bleibt das Spiel mehrsprachig.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Text {
    /// z. B. `error.not_flat`; die Übersetzungen liegen im Client.
    pub key: String,
    /// Platzhalter im Text, z. B. `{"max": "1"}` für `{max}`.
    pub args: BTreeMap<String, String>,
}

impl Text {
    pub fn new(key: &str) -> Text {
        Text { key: key.into(), args: BTreeMap::new() }
    }
    pub fn arg(mut self, name: &str, value: impl ToString) -> Text {
        self.args.insert(name.into(), value.to_string());
        self
    }
}

/// Eine Ware: Rohstoff, Zwischenprodukt oder Ausrüstung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Good {
    Wood,
    Boards,
    Stone,
    /// Quader vom Steinmetz, für hohe Mauern, Türme und den Bergfried.
    Blocks,
    IronOre,
    Iron,
    Grain,
    Flour,
    Bread,
    Fruit,
    Meat,
    Leather,
    Spear,
    Longbow,
    Crossbow,
    Sword,
    Armor,
    LightArmor,
    Saddle,
    /// Pferd aus der Pferdezucht. Bleibt in der Zucht, bis eine Einheit aufsteigt.
    Horse,
    /// Heilsalbe aus der Apotheke, für Heiler.
    Salve,
    /// Pfeile und Bolzen vom Pfeilmacher, für Bogen- und Armbrustschützen.
    Arrows,
}

/// Schadensart eines Angriffs. Jede Rüstung wehrt jede Art unterschiedlich gut ab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DamageType {
    /// Normaler Nahkampf (Speerträger, König).
    Melee,
    /// Schwerer Nahkampf (Schwertkämpfer, Ritter).
    HeavyMelee,
    /// Normaler Fernkampf (Bogen).
    Ranged,
    /// Schwerer Fernkampf (Armbrust).
    HeavyRanged,
    /// Belagerung (Belagerungsgeräte; der Ritter reißt so Mauern ein).
    Siege,
    /// Feuer (kommt noch: brennende Gebäude, Feuerpfeile).
    Fire,
}

impl DamageType {
    pub const ALL: [DamageType; 6] =
        [DamageType::Melee, DamageType::HeavyMelee, DamageType::Ranged, DamageType::HeavyRanged, DamageType::Siege, DamageType::Fire];
}

/// Ein Schadenswert einer Einheit: so viel Schaden `value` dieser Art je Treffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Damage {
    #[serde(rename = "type")]
    pub kind: DamageType,
    #[serde(default)]
    pub value: u32,
}

/// Rüstung gegen eine Schadensart: erst wird `min` abgezogen, vom Rest wehrt sie den Anteil
/// `block` ab (0 = nichts, 1 = alles, immun). Schaden = (X − min) × (1 − block).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Armor {
    #[serde(rename = "type")]
    pub kind: DamageType,
    #[serde(default)]
    pub min: u32,
    #[serde(default)]
    pub block: f32,
}

/// Menge einer Ware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GoodAmount {
    pub good: Good,
    pub amount: u32,
}

/// Lagerbestand eines Spielers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StockState {
    /// Nur Waren, von denen etwas da ist.
    pub goods: Vec<GoodAmount>,
    /// Gesamtplatz aller Lager zusammen.
    pub capacity: u32,
    /// Lagerplatz je Lagerart: jedes Lager hat einen Gesamtplatz, den sich alle
    /// Waren teilen, die es nimmt (Kornspeicher nur Essen, Waffenkammer nur Waffen …).
    #[serde(default)]
    pub pools: Vec<StoragePool>,
    #[serde(default)]
    pub people: PeopleState,
    /// Geld des Spielers.
    #[serde(default)]
    pub taler: u32,
    #[serde(default)]
    pub army: ArmyState,
    /// Automatischer Handel am Markt je Ware.
    #[serde(default)]
    pub auto_trade: Vec<AutoTrade>,
    /// Grenzen je Ware aus dem Ressourcen-Fenster: Startwerte für neue Betriebe.
    #[serde(default)]
    pub limits: Vec<GoodLimit>,
    /// Aktuelle Marktpreise (Preisdynamik); leer = die Grundpreise aus `market.prices`.
    #[serde(default)]
    pub prices: Vec<Price>,
}

/// Grenzen für eine Ware, die neue Betriebe übernehmen: Hersteller pausieren ab
/// `stop_at` im Vorrat, Verbraucher lassen `keep_in_stock` liegen. 0 = aus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GoodLimit {
    pub good: Good,
    #[serde(default)]
    pub stop_at: u32,
    #[serde(default)]
    pub keep_in_stock: u32,
}

/// Automatischer Handel für eine Ware: kaufen, solange weniger als `buy_below`
/// im Vorrat liegt, verkaufen, was über `sell_above` liegt. 0 = aus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AutoTrade {
    pub good: Good,
    #[serde(default)]
    pub buy_below: u32,
    #[serde(default)]
    pub sell_above: u32,
}

/// Lagerplatz einer Lagerart beim Spieler (alle Lager dieser Art zusammen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StoragePool {
    pub kind: BuildingKind,
    pub capacity: u32,
    pub used: u32,
    /// Waren, die diese Lager nehmen; leer = alle.
    pub goods: Vec<Good>,
}

/// Art eines Gebäudes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BuildingKind {
    /// Einfache Mauer, ein Feld.
    Wall,
    /// Hohe Mauer, ein Feld, schwerer zu überwinden.
    HighWall,
    /// Turm: niedriger, viel Platz für Einheiten.
    Tower,
    /// Hoher Turm: schmal und hoch, nur wenige Einheiten passen hinauf.
    HighTower,
    /// Bergfried: Hauptturm der Burg.
    Keep,
    /// Vorratslager: mehr Platz für Waren.
    Storehouse,
    Woodcutter,
    Sawmill,
    Quarry,
    Stonemason,
    Mine,
    Smelter,
    Smithy,
    SpearMaker,
    /// Pfeilmacher: Pfeile aus Holz, einfach zu bauen.
    Fletcher,
    Bowyer,
    Tannery,
    Farm,
    Mill,
    Bakery,
    /// Obstplantage: Obstbäume wachsen überall.
    Orchard,
    PigFarm,
    CattleFarm,
    /// Pferdezucht: züchtet Pferde aus Getreide; Reiter holen sie hier ab.
    HorseFarm,
    /// Wagenschuppen: stellt einen Karren für große Ladungen.
    CartShed,
    /// Wohnhaus: Platz für Arbeiter und Träger.
    House,
    /// Markt: Waren gegen Taler kaufen und verkaufen.
    Market,
    /// Tor in der Mauer: der mittlere Durchgang lässt sich öffnen und schließen.
    Gate,
    /// Holzmauer: billige Mauer aus Pfählen, hält weniger aus.
    Palisade,
    /// Treppenhaus an der Mauer: von hier steigen Einheiten auf den Wehrgang.
    Staircase,
    /// Kaserne: bildet Soldaten aus Waffen und Talern aus.
    Barracks,
    /// Waffenkammer: Lager nur für Waffen und Rüstungen.
    Armory,
    /// Kornspeicher: Lager nur für Lebensmittel.
    Granary,
    /// Weg aus Holzbohlen: darauf läuft man schneller.
    WoodPath,
    /// Gepflasterter Weg: darauf läuft man noch schneller.
    StonePath,
    /// Großes Torhaus: zwei Türme, dazwischen ein tiefer Durchgang mit Tor.
    Gatehouse,
    /// Apotheke: rührt Heilsalbe an und bildet daraus Heiler aus.
    Apothecary,
    /// Belagerungswerkstatt: bildet Baumeister aus; ihre Stufen schalten Belagerungsgeräte frei.
    SiegeWorkshop,
    /// Baustelle eines Belagerungsgeräts im Feld (entsteht nur über `build_siege`).
    SiegeSite,
    /// Belagerungslager: kleines Lager im Feld für Baustoffe und Steine. Karren füllen es
    /// aus der Burg auf, bis die eingestellten Mengen (`keep_in_stock`) dort liegen.
    SiegeCamp,
    /// Brücke über einen Fluss: eine gerade Reihe Wasserfelder (höchstens `max_span`),
    /// an beiden Enden Ufer. Kosten der Stufe gelten je Feld.
    Bridge,
}

/// Gruppe im Baumenü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BuildingCategory {
    Housing,
    Fortification,
    Storage,
    Raw,
    Workshop,
    Food,
    Military,
    Road,
}

/// Eine Ausbaustufe. Stufe 1 ist das Bauen selbst.
///
/// Alle Werte, die sich mit der Stufe ändern dürfen, stehen hier je Stufe vollständig
/// (Format 2 der Balancing-Datei). Fehlt ein Wert in einer Stufe, gilt der der Stufe davor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct BuildingLevel {
    /// Kosten für das Bauen (Stufe 1) bzw. den Ausbau auf diese Stufe.
    pub cost: Vec<GoodAmount>,
    /// Rezepte, die diese Stufe freischaltet (zusätzlich zu den vorigen).
    pub recipes: Vec<String>,
    /// So viele Bewohner passen auf dieser Stufe hinein (Wohnhäuser).
    pub housing: u32,
    /// Einheiten, die mit dieser Stufe dazukommen (Wagenschuppen: Träger, dann Karren).
    pub spawns: Vec<UnitCount>,
    /// Kaserne: Soldaten, die diese Stufe ausbilden kann (zusätzlich zu den vorigen).
    pub trains: Vec<UnitKind>,
    /// Belagerungswerkstatt: Geräte, die Baumeister ab dieser Stufe bauen dürfen (zusätzlich zu den vorigen).
    pub siege: Vec<UnitKind>,
    /// Lebenspunkte auf dieser Stufe.
    pub max_hp: u32,
    /// Arbeiter, die der Betrieb auf dieser Stufe braucht (sie wohnen in Häusern und essen).
    pub workers: u32,
    /// Wie weit um das Gebäude abgebaut wird (Felder vom Rand). 0 = nur unter dem Gebäude.
    pub gather_radius: u32,
    /// Wagenschuppen: der Karren fährt nur Betriebe in diesem Umkreis (Felder) an. 0 = überall.
    pub service_radius: u32,
    /// Auf dieser Stufe können eigene Einheiten oben auf dem Gebäude laufen (Wehrgang).
    pub walkway: bool,
    /// Tor/Turm: auf dieser Stufe führt eine Treppe auf den Wehrgang.
    pub stairs: bool,
    /// Tor: im Modus „automatisch“ schließt es, sobald fremde Einheiten so nah (Felder) sind.
    pub gate_radius: u32,
    /// So viele Einheiten finden oben Platz. 0 = so viele, wie draufpassen.
    pub garrison: u32,
    /// Deckung für Einheiten oben auf dem Gebäude: so viel Prozent der Fernkampf-Schüsse
    /// auf sie gehen daneben. 0 = keine Deckung.
    pub cover_percent: u32,
    /// Rüstung des Gebäudes auf dieser Stufe je Schadensart. Fehlt eine Art, trifft sie voll.
    pub armor: Vec<Armor>,
    /// Weg: man läuft hier so viel Prozent schneller. Ob es ein Weg ist, entscheidet Stufe 1 (> 0).
    pub road_speed_percent: u32,
    /// Platz je Ware in diesem Lager. 0 = wie bisher (`base_capacity` bzw. `storehouse_capacity`).
    pub hub_capacity: u32,
    /// Betriebe: so viel passt je Eingangsware ins eigene Lager. 0 = `transport.building_capacity`.
    pub storage_inputs: u32,
    /// Betriebe: so viel passt je Ergebnis ins eigene Lager. 0 = `transport.building_capacity`.
    pub storage_outputs: u32,
}

/// Wie ein neues Gebäude beliefert wird (Voreinstellung, der Spieler kann es je Gebäude ändern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct DeliveryDefaults {
    /// Träger liefern Eingangswaren (`settings.accept_deliveries`).
    pub carriers: bool,
    /// Karren füllen das Eingangslager auf (`settings.cart_deliveries`).
    pub carts: bool,
    /// Der Arbeiter trägt selbst (`settings.worker_carries`).
    pub worker: bool,
}

impl Default for DeliveryDefaults {
    fn default() -> Self {
        DeliveryDefaults { carriers: true, carts: true, worker: true }
    }
}

/// Wo ein Gebäude stehen darf: ein Mindestanteil seiner Felder muss einer
/// dieser Rohstoffe sein (z. B. Steinbruch auf Stein, Bauernhof auf Acker).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Placement {
    pub resources: Vec<ResourceKind>,
    pub min_share_percent: u32,
}

/// Feste Eigenschaften einer Gebäudeart. Was sich mit der Stufe ändern darf, steht in `levels`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildingDef {
    pub kind: BuildingKind,
    pub category: BuildingCategory,
    /// Grundfläche in Feldern.
    pub w: u32,
    pub h: u32,
    /// Höhe über dem Boden (Welt, 1 = ein Feld) für Pfeile: was darunter fliegt, stößt an.
    /// Je Ausbaustufe kommt ein Stockwerk dazu (nicht bei Befestigungen). 0 = Grundwert je Art.
    #[serde(default)]
    pub height: f32,
    /// Höchstens so viele pro Spieler.
    #[serde(default)]
    pub max_per_player: Option<u32>,
    /// Wo das Gebäude stehen darf. Fehlt es, reicht normaler Boden.
    #[serde(default)]
    pub placement: Option<Placement>,
    /// Rohstoff-Felder, die dieser Betrieb abbaut (z. B. Wald).
    #[serde(default)]
    pub gathers: Vec<ResourceKind>,
    /// Aufgeben hinterlässt diesen Rohstoff auf der Fläche (Obstplantage → Wald).
    #[serde(default)]
    pub releases_as: Option<ResourceKind>,
    /// So viele Spielsekunden nach dem Bau dürfen die Bäume erst als Wald freigegeben werden. 0 = sofort.
    #[serde(default)]
    pub release_after_seconds: f32,
    /// Höchstabstand zum eigenen Bergfried (Felder, ab dessen Mitte), gilt für alle Stufen:
    /// die ganze Fläche muss in diesem Umkreis liegen (beim Bauen und beim Ausbau). 0 = überall.
    #[serde(default)]
    pub build_radius: u32,
    /// Mauern und Türme haben keinen Eingang; alle anderen ein Feld davor, das frei bleiben muss.
    #[serde(default)]
    pub no_entrance: bool,
    /// Bergfried und Vorratslager: hier liegt der gemeinsame Vorrat, Träger liefern hierhin.
    #[serde(default)]
    pub hub: bool,
    /// Darf direkt auf eigene Mauern gesetzt werden; die Mauer darunter wird abgerissen.
    #[serde(default)]
    pub on_walls: bool,
    /// Darf auf Schrägen stehen (Mauern folgen dem Gelände).
    #[serde(default)]
    pub on_ramps: bool,
    /// Das erste Gebäude dieser Art je Spieler kostet nichts.
    #[serde(default)]
    pub first_free: bool,
    /// Lager nur für diese Waren (z. B. Waffenkammer). Leer = alle Waren.
    #[serde(default)]
    pub hub_goods: Vec<Good>,
    /// Voreinstellung für neue Gebäude: wer liefert (Träger, Karren, Arbeiter selbst).
    #[serde(default)]
    pub delivery: DeliveryDefaults,
    /// Pferdezucht: so viele Pferde passen hinein. 0 = keine Pferde.
    #[serde(default)]
    pub horse_capacity: u32,
    /// Brücke: höchstens so viele Wasserfelder lang (0 = 15).
    #[serde(default)]
    pub max_span: u32,
    /// Kein Bauen, Ausbauen oder Reparieren, solange ein Feind (Soldat, König) so nah ist (Felder, 0 = aus).
    #[serde(default = "default_enemy_radius")]
    pub enemy_radius: f32,
    /// Mindestens eine Stufe.
    pub levels: Vec<BuildingLevel>,
}

impl BuildingDef {
    /// Werte der Stufe `level` (beginnt bei 1; außerhalb: nächste vorhandene Stufe).
    pub fn at(&self, level: u32) -> &BuildingLevel {
        let i = (level.max(1) as usize - 1).min(self.levels.len().saturating_sub(1));
        &self.levels[i]
    }

    /// Ist es ein Weg (das Feld bleibt begehbar)? Entscheidet Stufe 1.
    pub fn is_road(&self) -> bool {
        self.levels.first().is_some_and(|l| l.road_speed_percent > 0)
    }

    /// Hat irgendeine Stufe einen Wehrgang?
    pub fn any_walkway(&self) -> bool {
        self.levels.iter().any(|l| l.walkway)
    }

    /// Größter Wert über alle Stufen (z. B. für Kreise beim Bauen).
    pub fn max_of(&self, f: impl Fn(&BuildingLevel) -> u32) -> u32 {
        self.levels.iter().map(f).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnitCount {
    pub kind: UnitKind,
    pub count: u32,
}

/// Werte für Träger und Karren.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransportDef {
    pub carrier_capacity: u32,
    /// Felder pro Sekunde.
    pub carrier_speed: f32,
    pub cart_capacity: u32,
    pub cart_speed: f32,
    /// Der Karren fährt erst ab dieser Menge los (sonst lohnt es sich nicht).
    pub cart_min_load: u32,
    /// So viel einer Ware passt in das Lager eines Betriebs.
    pub building_capacity: u32,
}

/// Ein Rezept: was ein Betrieb aus welchen Waren macht.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecipeDef {
    pub id: String,
    pub building: BuildingKind,
    #[serde(default)]
    pub inputs: Vec<GoodAmount>,
    pub outputs: Vec<GoodAmount>,
    /// Dauer eines Durchlaufs.
    pub seconds: f32,
    /// So viel Prozent am Anfang des Durchlaufs laufen ohne Arbeiter weiter
    /// (Wachsen von Getreide, Obst, Tieren), auch wenn er gerade Waren trägt.
    #[serde(default)]
    pub unattended_percent: u32,
}

/// Die ganze Wirtschaft als Daten (einzige Quelle: `server/data/economy.json`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EconomyDef {
    /// Aufbau der Datei: 2 = Werte je Gebäudestufe. Ältere Dateien (ohne) werden beim Laden umgewandelt.
    #[serde(default)]
    pub format: u32,
    pub goods: Vec<Good>,
    pub buildings: Vec<BuildingDef>,
    pub recipes: Vec<RecipeDef>,
    pub start_stock: Vec<GoodAmount>,
    /// Steht beim Start schon an der Fahne des Spielers (z. B. der Bergfried).
    #[serde(default)]
    pub start_building: Option<BuildingKind>,
    /// Lagerplatz je Ware ohne Vorratslager.
    pub base_capacity: u32,
    /// Zusätzlicher Platz je Ware pro Vorratslager.
    pub storehouse_capacity: u32,
    /// So viele Stück einer Ware passen auf einen Lagerplatz (fehlt sie: 1). Pfeile z. B. 10.
    #[serde(default)]
    pub per_slot: Vec<GoodAmount>,
    /// So viel Prozent der Bau- und Ausbaukosten gibt es beim Abriss zurück.
    pub demolish_refund_percent: u32,
    /// So viel Prozent der Bau- und Ausbaukosten kostet das Versetzen eines Gebäudes.
    #[serde(default = "default_move_cost")]
    pub move_cost_percent: u32,
    /// Gebäude ohne `on_ramps` dürfen trotzdem mit so vielen Feldern auf einer Schräge
    /// oder eine Stufe neben der Grundhöhe stehen (0 = ganz eben).
    #[serde(default = "default_slope_tiles")]
    pub slope_tiles: u32,
    /// Wie viel ein Feld hergibt, bevor es erschöpft ist (Wald, Stein, Eisen).
    pub resource_amounts: Vec<ResourceAmount>,
    /// So viel schneller arbeiten Höfe auf besonders fruchtbarem Boden (anteilig).
    pub fertile_bonus_percent: u32,
    pub transport: TransportDef,
    #[serde(default)]
    pub movement: MovementDef,
    #[serde(default)]
    pub population: PopulationDef,
    #[serde(default)]
    pub market: MarketDef,
    /// Soldaten: Kosten, Ausbildungszeit, Werte.
    #[serde(default)]
    pub units: Vec<UnitDef>,
    #[serde(default)]
    pub army: ArmyDef,
}

fn yes() -> bool {
    true
}

/// Eine Soldatenart, wie sie die Kaserne ausbildet.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnitDef {
    pub kind: UnitKind,
    /// Ausrüstung aus dem Vorrat (Waffen, Rüstung, Sattel).
    #[serde(default)]
    pub cost: Vec<GoodAmount>,
    /// Handgeld in Talern.
    #[serde(default)]
    pub taler: u32,
    /// Ausbildungszeit in Sekunden.
    pub seconds: f32,
    /// Felder pro Sekunde.
    pub speed: f32,
    #[serde(default)]
    pub hp: u32,
    /// Größter Schaden je Treffer (aus `damage`; nur wenn `damage` leer ist, zählt dieser Wert
    /// selbst, als Nah- oder Fernkampf je nach Reichweite).
    #[serde(default)]
    pub attack: u32,
    /// Schaden je Treffer nach Art. Gegen ein Ziel zählt die Art, die nach dessen Rüstung
    /// am meisten anrichtet.
    #[serde(default)]
    pub damage: Vec<Damage>,
    /// Rüstung der Einheit je Schadensart. Fehlt eine Art, trifft sie voll.
    #[serde(default)]
    pub armor: Vec<Armor>,
    /// Rüstung des Pferdes, wenn die Einheit reitet.
    #[serde(default)]
    pub horse_armor: Vec<Armor>,
    /// Reichweite in Feldern, 1 = Nahkampf (für den Kampf, kommt noch).
    #[serde(default)]
    pub range: f32,
    /// Streuradius am Einschlag (Felder) auf volle Reichweite, näher entsprechend weniger.
    /// 0 = trifft genau den Zielpunkt (die Schützen halten trotzdem vor).
    #[serde(default)]
    pub scatter: f32,
    /// Sold in Talern je Zahltag (siehe `army.upkeep_seconds`).
    #[serde(default)]
    pub upkeep: u32,
    /// Essen je Zahltag (Einheiten aus `population.food`). Fehlt es, laufen die Soldaten langsamer.
    #[serde(default)]
    pub food: u32,
    /// Kann auf ein Pferd aus der Pferdezucht steigen.
    #[serde(default)]
    pub can_ride: bool,
    /// Kommt beritten aus der Kaserne (Ritter): die Ausbildung braucht ein Pferd aus einer Pferdezucht.
    #[serde(default)]
    pub mounted: bool,
    /// Greift an, wenn sie zu Fuß steht.
    #[serde(default = "yes")]
    pub attack_standing: bool,
    /// Greift auch im Laufen an (zu Fuß), ohne anzuhalten.
    #[serde(default)]
    pub attack_moving: bool,
    /// Greift zu Pferd an (im Stehen und im Reiten).
    #[serde(default = "yes")]
    pub attack_mounted: bool,
    /// Sekunden zwischen zwei Schlägen oder Schüssen.
    #[serde(default)]
    pub attack_seconds: f32,
    /// Heiler: so viele Lebenspunkte gibt er je Behandlung zurück (alle `army.heal_seconds`). 0 = heilt nicht.
    #[serde(default)]
    pub heal: u32,
    /// Heiler: so weit (Felder) reicht er zu einem Verwundeten.
    #[serde(default)]
    pub heal_range: f32,
    /// Vorrat an Schüssen (Köcher, Steine im Gerät); ist er leer, schießt die Einheit erst nach
    /// Nachschub wieder. 0 = unbegrenzt. Gilt nur, wenn die Spielregel Munition verlangt.
    #[serde(default)]
    pub shots: u32,
    /// Nachschub je Schuss (Karren bringen ihn, Schützen holen ihn auch am nahen Lager).
    /// Ohne: unbegrenzt.
    #[serde(default)]
    pub ammo: Option<GoodAmount>,
    /// Belagerungsgerät: Bau und Bedienung. `cost` sind dann die Baustoffe, die zur Baustelle
    /// gebracht werden, `seconds` die Bauzeit für einen Baumeister.
    #[serde(default)]
    pub siege: Option<SiegeDef>,
}

fn amount(list: &[GoodAmount], good: Good) -> u32 {
    list.iter().filter(|g| g.good == good).map(|g| g.amount).sum()
}

fn add(list: &mut Vec<GoodAmount>, good: Good, n: u32) {
    match list.iter_mut().find(|g| g.good == good) {
        Some(g) => g.amount += n,
        None => list.push(GoodAmount { good, amount: n }),
    }
}

impl SiteState {
    /// Wie viel von `good` fehlt noch?
    pub fn missing(&self, good: Good) -> u32 {
        amount(&self.needed, good).saturating_sub(amount(&self.delivered, good))
    }

    /// Anteil der gelieferten Baustoffe (1 = alles da).
    pub fn supplied(&self) -> f32 {
        let total: u32 = self.needed.iter().map(|g| g.amount).sum();
        if total == 0 {
            return 1.0;
        }
        let have: u32 = self.needed.iter().map(|g| amount(&self.delivered, g.good).min(g.amount)).sum();
        have as f32 / total as f32
    }

    /// Bringt Baustoffe; liefert, wie viel davon gebraucht wurde.
    pub fn deliver(&mut self, load: GoodAmount) -> u32 {
        let n = load.amount.min(self.missing(load.good));
        if n > 0 {
            add(&mut self.delivered, load.good, n);
        }
        n
    }
}

/// Was ein Belagerungsgerät ausmacht (siehe `UnitDef::siege`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct SiegeDef {
    /// Seitenlänge der Baustelle in Feldern (nicht bei `on_towers`).
    pub size: u32,
    /// So viele Baumeister müssen es bedienen, sonst schießt und fährt es nicht.
    pub crew: u32,
    /// Näher als so viele Felder schießt es nicht.
    pub min_range: f32,
    /// Sucht sich Einheiten von selbst als Ziel. Aus: auf Einheiten nur mit Angriffsbefehl.
    pub auto_units: bool,
    /// Einheiten in diesem Umkreis (Felder) um den Einschlag werden alle getroffen. 0 = nur eine.
    pub splash: f32,
    /// Schießt auf Gebäude (Mauern, Türme …).
    pub hits_buildings: bool,
    /// Schießt auf Einheiten.
    pub hits_units: bool,
    /// Wird oben auf einem eigenen Turm gebaut und bleibt dort.
    pub on_towers: bool,
    /// So schnell fliegt das Geschoss (Felder pro Sekunde).
    pub shot_speed: f32,
    /// Belagerungsturm: dockt an Mauern mit Wehrgang an und wird zur Treppe hinauf.
    pub docks: bool,
}

impl Default for SiegeDef {
    fn default() -> Self {
        SiegeDef {
            size: 2,
            crew: 1,
            min_range: 0.0,
            auto_units: true,
            splash: 0.0,
            hits_buildings: true,
            hits_units: true,
            on_towers: false,
            shot_speed: 15.0,
            docks: false,
        }
    }
}

/// Baustelle eines Belagerungsgeräts (an der Baustelle im Feld oder am Turm der Balliste).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SiteState {
    /// Was hier entsteht.
    pub unit: UnitKind,
    /// Baustoffe insgesamt …
    pub needed: Vec<GoodAmount>,
    /// … und was davon schon da ist (Karren und mitgebrachte Rucksäcke).
    pub delivered: Vec<GoodAmount>,
    /// Geleistete Arbeit und die nötige (Sekunden für einen Baumeister).
    pub work: f32,
    pub work_total: f32,
    /// Baumeister, die gerade hier arbeiten.
    pub builders: u32,
}

/// Regeln für das Heer.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct ArmyDef {
    /// Alle so viele Sekunden ist Zahltag: jeder Soldat bekommt seinen Sold. 0 = nie.
    pub upkeep_seconds: f32,
    /// Reicht das Geld nicht, laufen alle Soldaten nur mit diesem Anteil des Tempos.
    pub unpaid_speed_percent: u32,
    /// So viele Soldaten kann eine Kaserne höchstens in der Warteschlange haben.
    pub queue_max: u32,
    /// Bis zu dieser Reichweite (Felder) zählt ein Angriff als Nahkampf, darüber als Fernkampf.
    pub melee_range: f32,
    /// So viele Sekunden nach dem Tod eines Arbeiters, Trägers oder Karrens kommt Ersatz
    /// (aus einem Wohnhaus bzw. am eigenen Gebäude). 0 = nie.
    pub civilian_respawn_seconds: f32,
    /// Bauen, Ausbauen und Reparieren nur, wenn in diesem Umkreis (Felder) kein feindlicher
    /// Soldat oder König steht. 0 = immer erlaubt.
    pub enemy_block_radius: f32,
    /// Lebenspunkte eines Pferdes. Treffer gegen einen Reiter gehen zu `horse_hit_percent` aufs Pferd.
    pub horse_hp: u32,
    /// Tempo zu Pferd (Felder pro Sekunde); wer zu Fuß schneller ist, bleibt so schnell.
    pub horse_speed: f32,
    /// Fällt das Pferd, nimmt der Reiter beim Sturz so viel Schaden …
    pub fall_damage: u32,
    /// … und liegt so viele Sekunden am Boden: kann nicht laufen und nicht kämpfen.
    pub fall_seconds: f32,
    /// Sekunden zwischen zwei Behandlungen eines Heilers.
    pub heal_seconds: f32,
    /// Warnung, sobald ein feindlicher Soldat oder König so nah (Felder) am eigenen Bergfried ist. 0 = aus.
    pub alert_radius: f32,
    /// Nach einer Warnung kommt dieselbe Art frühestens nach so vielen Sekunden wieder.
    pub alert_seconds: f32,
    /// Mindestschaden je Treffer, wenn die Rüstung fast alles abfängt (nicht bei voller Abwehr).
    pub min_damage: u32,
    /// Treffer gegen einen Reiter gehen zu so viel Prozent aufs Pferd, sonst auf den Reiter.
    pub horse_hit_percent: u32,
    /// Höhenvorteil, Nahkampf: zwischen Angreifer und Ziel höchstens so viele Geländestufen
    /// Unterschied (Mauern zählen mit). Mehr = kein Nahkampf möglich.
    pub melee_max_levels: f32,
    /// Höhenvorteil, Fernkampf: so viel Prozent mehr Reichweite je Stufe, die der Schütze
    /// höher steht als sein Ziel (Gelände, Mauer, Turm). Gleiche Höhe = kein Bonus.
    pub height_range_percent: f32,
    /// Fernkampf: so schnell (Felder pro Sekunde entlang der Bahn) fliegen Pfeile und Bolzen.
    /// Schaden gibt es erst beim Aufschlag; Schützen halten auf laufende Ziele vor.
    pub arrow_speed: f32,
    /// Fernkampf: so nah (Felder) muss ein Ziel beim Aufschlag am Einschlagpunkt stehen, sonst geht der Pfeil daneben.
    pub arrow_hit_radius: f32,
    /// Beute: wer einen König tötet, bekommt so viel Prozent der Waren und Taler des Besiegten.
    pub loot_percent: u32,
    /// Belagerung: so viele Baustoffe trägt ein Baumeister im Rucksack zur Baustelle („provisorisch mitnehmen“).
    pub builder_backpack: u32,
    /// Munition: Schützen und Geräte so nah (Felder) an einem eigenen Lager füllen ihren
    /// Vorrat dort selbst auf (sonst bringen Karren Nachschub). 0 = nie.
    pub refill_radius: f32,
}

impl Default for ArmyDef {
    fn default() -> Self {
        ArmyDef {
            upkeep_seconds: 60.0,
            unpaid_speed_percent: 60,
            queue_max: 10,
            melee_range: 2.0,
            civilian_respawn_seconds: 30.0,
            enemy_block_radius: 50.0,
            horse_hp: 40,
            horse_speed: 2.4,
            fall_damage: 15,
            fall_seconds: 8.0,
            heal_seconds: 2.0,
            alert_radius: 80.0,
            alert_seconds: 30.0,
            min_damage: 1,
            horse_hit_percent: 70,
            melee_max_levels: 1.0,
            height_range_percent: 5.0,
            arrow_speed: 22.0,
            arrow_hit_radius: 0.6,
            loot_percent: 50,
            builder_backpack: 5,
            refill_radius: 10.0,
        }
    }
}

/// Heer eines Spielers, wie im Vorrat angezeigt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArmyState {
    pub soldiers: u32,
    /// Sold aller Soldaten je Zahltag.
    pub upkeep: u32,
    /// Am letzten Zahltag reichte das Geld nicht.
    pub unpaid: bool,
    /// Essen aller Soldaten je Zahltag.
    #[serde(default)]
    pub food: u32,
    /// Am letzten Zahltag reichte das Essen nicht.
    #[serde(default)]
    pub hungry: bool,
}

/// Lauftempo der Einheiten, die nicht transportieren (Felder pro Sekunde).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MovementDef {
    pub king_speed: f32,
    /// Arbeiter der Abbaubetriebe.
    #[serde(default = "default_worker_speed")]
    pub worker_speed: f32,
    /// Tempo auf Schrägen in Prozent (100 = wie flach). Langsamer macht Umwege um Berge lohnend.
    #[serde(default = "default_slope_speed")]
    pub slope_speed_percent: u16,
    /// Mindestabstand stehender Soldaten zueinander (Felder); enger stehende rücken auseinander.
    /// Reiter brauchen das 1,5-Fache. 0 = aus.
    #[serde(default = "default_spread_distance")]
    pub spread_distance: f32,
    /// So schnell rücken sie dabei (Felder pro Sekunde).
    #[serde(default = "default_spread_speed")]
    pub spread_speed: f32,
    /// Weg-Neuberechnung nach Bauen/Abreißen: wird ein Weg versperrt, sucht die Einheit sofort
    /// neu. Alle anderen (kürzere Wege durch neue Lücken, Wartende) erst so viele Sekunden nach
    /// der letzten Änderung, höchstens nach der doppelten Zeit. 0 = immer sofort.
    #[serde(default = "default_replan_delay")]
    pub replan_delay_seconds: f32,
    /// Größter Höhenunterschied (Stufen) zwischen zwei benachbarten Wehrgangfeldern, über den
    /// Einheiten oben weiterlaufen (Mauern am Hang). Größer = getrennte Wehrgänge.
    #[serde(default = "default_walkway_step")]
    pub walkway_step_levels: u8,
}

fn default_walkway_step() -> u8 {
    2
}

fn default_replan_delay() -> f32 {
    5.0
}

fn default_spread_distance() -> f32 {
    0.5
}

fn default_spread_speed() -> f32 {
    1.0
}

fn default_slope_speed() -> u16 {
    60
}

fn default_worker_speed() -> f32 {
    1.2
}

/// Bewohner: Arbeiter und Träger brauchen einen Platz in einem Haus und essen.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PopulationDef {
    /// Diese Waren werden gegessen (eine Einheit je Bewohner und Mahlzeit).
    pub food: Vec<Good>,
    /// Alle so viele Sekunden isst jeder Bewohner einmal.
    pub meal_seconds: f32,
    /// Ohne Essen arbeiten und laufen alle nur mit diesem Anteil des Tempos.
    pub hungry_speed_percent: u32,
    /// Bewohner je Träger oder Karren.
    pub per_transport: u32,
    /// Rationen, die ein Spieler wählen kann: so viel Essen je Bewohner und Mahlzeit,
    /// und so viel schneller arbeiten und laufen alle, die ihre volle Ration bekommen.
    #[serde(default = "default_rations")]
    pub rations: Vec<RationLevel>,
    /// Mit dieser Ration (Index in `rations`) fängt jeder an.
    #[serde(default)]
    pub default_ration: u32,
}

/// Eine wählbare Ration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RationLevel {
    /// Essen je Bewohner und Mahlzeit (Träger und Karren mal `per_transport`).
    pub food: u32,
    /// Tempo-Zuschlag in Prozent für alle, die die volle Ration bekommen.
    pub speed_percent: i32,
}

/// Ein wählbarer Steuersatz.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaxLevel {
    /// So viel Prozent von `tax_per_meal` zahlt jeder satte Bewohner je Mahlzeit.
    pub tax_percent: u32,
    /// Tempo-Zuschlag (oder mit Minus Abzug) in Prozent für alle satten Bewohner.
    pub speed_percent: i32,
}

fn default_rations() -> Vec<RationLevel> {
    vec![
        RationLevel { food: 1, speed_percent: 0 },
        RationLevel { food: 2, speed_percent: 15 },
        RationLevel { food: 3, speed_percent: 25 },
    ]
}

fn default_tax_levels() -> Vec<TaxLevel> {
    vec![
        TaxLevel { tax_percent: 0, speed_percent: 15 },
        TaxLevel { tax_percent: 50, speed_percent: 8 },
        TaxLevel { tax_percent: 100, speed_percent: 0 },
        TaxLevel { tax_percent: 200, speed_percent: -10 },
        TaxLevel { tax_percent: 300, speed_percent: -25 },
    ]
}

fn default_tax_level() -> u32 {
    2
}

impl Default for PopulationDef {
    fn default() -> Self {
        PopulationDef {
            food: vec![Good::Bread, Good::Meat, Good::Fruit],
            meal_seconds: 60.0,
            hungry_speed_percent: 50,
            per_transport: 1,
            rations: default_rations(),
            default_ration: 0,
        }
    }
}

/// Taler und Markt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MarketDef {
    /// Taler zu Spielbeginn.
    pub start_taler: u32,
    /// So viele Taler zahlt jeder satte Bewohner pro Mahlzeit an Steuern.
    pub tax_per_meal: u32,
    /// Steuersätze, die ein Spieler wählen kann (Anteil von `tax_per_meal`, Wirkung aufs Tempo).
    #[serde(default = "default_tax_levels")]
    pub tax_levels: Vec<TaxLevel>,
    /// Mit diesem Steuersatz (Index in `tax_levels`) fängt jeder an.
    #[serde(default = "default_tax_level")]
    pub default_tax: u32,
    /// Preise je Ware. Fehlt eine Ware, kann man sie nicht handeln.
    pub prices: Vec<Price>,
    /// Preisdynamik: jedes gekaufte Stück hebt den Preis der Ware um so viel Prozent,
    /// jedes verkaufte senkt ihn (für alle Spieler gemeinsam). 0 = feste Preise.
    #[serde(default = "default_price_step")]
    pub price_step_percent: f32,
    /// So viel Prozent ihres Abstands zum Grundpreis holen die Preise je Minute wieder auf.
    #[serde(default = "default_price_recovery")]
    pub price_recovery_percent: f32,
    /// Untere und obere Grenze der Preise in Prozent des Grundpreises.
    #[serde(default = "default_price_min")]
    pub price_min_percent: f32,
    #[serde(default = "default_price_max")]
    pub price_max_percent: f32,
}

fn default_price_step() -> f32 {
    0.5
}

fn default_price_recovery() -> f32 {
    20.0
}

fn default_price_min() -> f32 {
    30.0
}

fn default_price_max() -> f32 {
    300.0
}

/// Preis einer Ware am Markt: so viel zahlt man beim Kauf, so viel bekommt man beim Verkauf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Price {
    pub good: Good,
    pub buy: u32,
    pub sell: u32,
}

/// Bewohner eines Spielers, wie im Vorrat angezeigt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeopleState {
    /// Plätze in allen Häusern.
    pub housing: u32,
    /// So viele Arbeiter brauchen alle Betriebe, Träger und Karren zusammen.
    pub needed: u32,
    /// So viele davon haben eine Wohnung und arbeiten.
    pub working: u32,
    /// Mindestens ein Bewohner hat bei seiner letzten Mahlzeit nichts bekommen.
    pub hungry: bool,
    /// So viele Bewohner hungern gerade (von `working`; nur sie sind langsamer).
    #[serde(default)]
    pub hungry_count: u32,
    /// So viele sind satt (von `working`).
    #[serde(default)]
    pub fed: u32,
    /// Sekunden (Spielzeit) bis zur nächsten Mahlzeit, Stand beim Senden.
    #[serde(default)]
    pub next_meal_in: u32,
    /// Gewählte Ration (Index in `population.rations`).
    #[serde(default)]
    pub ration: u32,
    /// Gewählter Steuersatz (Index in `market.tax_levels`).
    #[serde(default)]
    pub tax: u32,
    /// So viele haben zuletzt ihre volle Ration bekommen (von `fed`).
    #[serde(default)]
    pub well_fed: u32,
}

impl Default for MovementDef {
    fn default() -> Self {
        MovementDef {
            king_speed: 1.5,
            worker_speed: 1.2,
            slope_speed_percent: default_slope_speed(),
            spread_distance: default_spread_distance(),
            spread_speed: default_spread_speed(),
            replan_delay_seconds: default_replan_delay(),
            walkway_step_levels: default_walkway_step(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResourceAmount {
    pub resource: ResourceKind,
    pub amount: u32,
}

/// Was der Spieler pro Gebäude einstellt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildingSettings {
    /// Welches freigeschaltete Rezept läuft. `None` = nichts herstellen.
    pub recipe: Option<String>,
    pub paused: bool,
    /// Darf der Transport hier Eingangswaren abliefern? Aus = der Betrieb
    /// arbeitet nur mit dem, was schon in seinem Lager liegt.
    pub accept_deliveries: bool,
    /// Lager: welche Waren hier eingelagert werden. Wagenschuppen: welche Waren
    /// seine Träger und Karren bewegen. `None` = alle.
    #[serde(default)]
    pub stores: Option<Vec<Good>>,
    /// Mindestbestand im Vorrat: von diesen Waren bekommt der Betrieb nur, was
    /// darüber liegt (z. B. Holz fürs Bauen behalten).
    #[serde(default)]
    pub keep_in_stock: Vec<GoodAmount>,
    /// Nur Tore: automatisch, fest offen oder fest zu.
    #[serde(default)]
    pub gate: GateMode,
    /// Der Arbeiter bringt Ergebnisse selbst ins Lager und holt Eingänge selbst,
    /// auch wenn es Träger gibt. Standard: an.
    #[serde(default = "default_true")]
    pub worker_carries: bool,
    /// Höchstbestand im Vorrat: liegt von einem Ergebnis so viel im Vorrat, macht der Betrieb Pause.
    #[serde(default)]
    pub stop_at: Vec<GoodAmount>,
    /// Karren füllen das Eingangslager auf (auch wenn der Arbeiter selbst trägt). Standard: an.
    #[serde(default)]
    pub cart_deliveries: bool,
}

/// Wie sich ein Tor verhält.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GateMode {
    /// Offen, schließt aber, sobald fremde Einheiten nahe kommen.
    #[default]
    Auto,
    Open,
    Closed,
}

/// Was ein Betrieb gerade tut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BuildingStatus {
    /// Stellt nichts her (z. B. Mauer, oder kein Rezept gewählt).
    Idle,
    Working,
    Paused,
    /// Es fehlen Eingangswaren im Lager.
    MissingInput,
    /// Kein Rohstoff mehr in Reichweite (Wald abgeholzt, Stein erschöpft).
    NoResource,
    /// Lager des Betriebs voll, das Ergebnis passt nicht hinein (Träger fehlen?).
    StorageFull,
    /// Lager des Betriebs voll, und in keinem eigenen Lager ist Platz für das Ergebnis.
    NoStorageRoom,
    /// Keine Wohnung frei für die Arbeiter: mehr Häuser bauen.
    NoWorkers,
    /// Der Arbeiter trägt gerade selbst Waren (kein Wagenschuppen mit Trägern).
    WorkerAway,
    /// Höchstbestand im Vorrat erreicht (`settings.stop_at`): Pause, bis wieder Platz ist.
    TargetReached,
    /// Eingangswaren liegen nur noch bis zum Mindestbestand (`settings.keep_in_stock`)
    /// im Vorrat: der Betrieb wartet, bis wieder mehr da ist.
    StockReserved,
}

/// Ein Gebäude auf der Karte. Es belegt die Felder (x, y) bis (x + w - 1, y + h - 1).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildingState {
    pub id: u32,
    pub owner: u32,
    pub kind: BuildingKind,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub hp: u32,
    /// Ausbaustufe, beginnt bei 1.
    pub level: u32,
    pub settings: BuildingSettings,
    pub status: BuildingStatus,
    /// Vierteldrehungen wie bei `place_building`. `w`/`h` sind schon gedreht.
    #[serde(default)]
    pub rotation: u8,
    /// Feld vor dem Eingang: hier holen und bringen Träger. `None` bei Mauern und Türmen.
    #[serde(default)]
    pub entrance: Option<(u32, u32)>,
    /// Waren im Gebäude selbst: Eingänge für den nächsten Durchlauf und
    /// Ergebnisse, die noch abgeholt werden. Leer bei Lagern (die zählen zum Vorrat).
    #[serde(default)]
    pub inventory: Vec<GoodAmount>,
    /// Nur Tore: ist der Durchgang gerade offen?
    #[serde(default)]
    pub gate_open: bool,
    /// Nur Kaserne: Soldaten in der Ausbildung, der erste wird gerade ausgebildet.
    #[serde(default)]
    pub training: Vec<UnitKind>,
    /// Sekunden (Spielzeit), bis der erste fertig ist. Der Client zählt selbst herunter.
    #[serde(default)]
    pub training_left: f32,
    /// Laufender Durchlauf: wie weit (0..1). Der Client rechnet mit
    /// `progress_rate` (Anteil je Spielsekunde) selbst weiter.
    #[serde(default)]
    pub progress: f32,
    #[serde(default)]
    pub progress_rate: f32,
    /// Zählt jede neue Meldung von `progress`/`progress_rate` hoch, damit der Client
    /// auch dann neu ansetzt, wenn ein neuer Durchlauf mit denselben Werten beginnt.
    #[serde(default)]
    pub progress_seq: u32,
    /// Tor auf einem Weg gebaut: so viel % schneller ist der Durchgang (0 = kein Weg).
    #[serde(default)]
    pub road_under: u32,
    /// Nur Lager: so viel passt insgesamt hinein (belegt = Summe von `inventory`).
    #[serde(default)]
    pub capacity: u32,
    /// Obstplantage: noch so viele Spielsekunden, bis die Bäume als Wald freigegeben werden dürfen. 0 = jetzt.
    #[serde(default)]
    pub mature_in: f32,
    /// Nur Kaserne: Sammelpunkt (Feldkoordinaten). Neue Soldaten laufen hierhin. `None` = vor die Tür.
    #[serde(default)]
    pub rally: Option<(f32, f32)>,
    /// Baustelle eines Belagerungsgeräts (Baustelle im Feld, Turm mit Balliste im Bau).
    #[serde(default)]
    pub site: Option<SiteState>,
}

fn default_enemy_radius() -> f32 {
    15.0
}

fn default_true() -> bool {
    true
}

fn default_slope_tiles() -> u32 {
    2
}

fn default_move_cost() -> u32 {
    25
}
