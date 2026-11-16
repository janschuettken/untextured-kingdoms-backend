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
    MoveUnits { unit_ids: Vec<u32>, x: f32, y: f32 },
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
    /// Eigenes Gebäude abreißen. Ein Teil der Baukosten kommt zurück.
    DemolishBuilding { building_id: u32 },
    /// Eigenes Gebäude auf die nächste Stufe ausbauen.
    UpgradeBuilding { building_id: u32 },
    /// Einstellungen eines eigenen Gebäudes ändern (was es herstellt usw.).
    ConfigureBuilding { building_id: u32, settings: BuildingSettings },
    /// Obstplantage aufgeben: ihre Bäume werden zu Wald, den man abholzen kann.
    ReleaseTrees { building_id: u32 },
    /// Diese Einheit beobachten: der Server schickt ihren Weg (`UnitPath`). `None` = keine.
    WatchUnit { unit_id: Option<u32> },
    /// Laufkosten aller Felder anfordern (Heatmap). Antwort: `NavCosts`.
    RequestNavCosts,
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
    },
    Tick {
        /// JSON-Zahl; reicht bei 20 Ticks/s für Millionen Jahre.
        #[ts(type = "number")]
        tick: u64,
        players_online: u32,
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
    },
    /// Der König dieses Spielers ist tot: er scheidet aus, alles von ihm ist verschwunden.
    /// Geht an alle; ein ausgeschiedener Spieler bekommt es beim Verbinden noch einmal.
    PlayerDefeated {
        player: u32,
        /// Wessen Einheit den letzten Schlag geführt hat.
        by: Option<u32>,
    },
    /// Nur noch ein Spieler (oder ein Bündnis) hat seinen König: das Spiel ist entschieden.
    GameOver {
        winners: Vec<u32>,
    },
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
    /// Das eigene Spiel verlassen (wartend oder laufend).
    LeaveGame,
    /// Startplatz wechseln (nur auf einen freien).
    TakeSlot { slot: u32 },
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
    /// Nur Gastgeber, nur vor dem Start: wie Waren zwischen Verbündeten ankommen.
    SetTradeMode { mode: TradeMode },
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
    /// Antwort auf `hello`: die eigene öffentliche Id und die Karten zur Auswahl.
    Welcome { you: String, maps: Vec<String> },
    /// Kompletter Stand, kommt bei jeder Änderung neu.
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
    pub map: String,
    pub phase: GamePhase,
    pub host_name: String,
    pub players: u32,
    pub max_players: u32,
}

/// Das Spiel, in dem man selbst ist, mit allen Startplätzen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LobbyGame {
    pub id: u32,
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
}

impl UnitKind {
    /// Soldaten: werden in der Kaserne ausgebildet und kosten Sold.
    pub fn is_soldier(self) -> bool {
        matches!(self, UnitKind::Spearman | UnitKind::Archer | UnitKind::Crossbowman | UnitKind::LightSwordsman | UnitKind::Swordsman | UnitKind::Knight)
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
    Bowyer,
    Tannery,
    Farm,
    Mill,
    Bakery,
    /// Obstplantage: Obstbäume wachsen überall.
    Orchard,
    PigFarm,
    CattleFarm,
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
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildingLevel {
    /// Kosten für das Bauen (Stufe 1) bzw. den Ausbau auf diese Stufe.
    pub cost: Vec<GoodAmount>,
    /// Rezepte, die diese Stufe freischaltet (zusätzlich zu den vorigen).
    #[serde(default)]
    pub recipes: Vec<String>,
    /// So viele Bewohner passen auf dieser Stufe hinein (Wohnhäuser).
    #[serde(default)]
    pub housing: u32,
    /// Einheiten, die mit dieser Stufe dazukommen (Wagenschuppen: Träger, dann Karren).
    #[serde(default)]
    pub spawns: Vec<UnitCount>,
    /// Kaserne: Soldaten, die diese Stufe ausbilden kann (zusätzlich zu den vorigen).
    #[serde(default)]
    pub trains: Vec<UnitKind>,
}

/// Wo ein Gebäude stehen darf: ein Mindestanteil seiner Felder muss einer
/// dieser Rohstoffe sein (z. B. Steinbruch auf Stein, Bauernhof auf Acker).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Placement {
    pub resources: Vec<ResourceKind>,
    pub min_share_percent: u32,
}

/// Feste Eigenschaften einer Gebäudeart.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildingDef {
    pub kind: BuildingKind,
    pub category: BuildingCategory,
    /// Grundfläche in Feldern.
    pub w: u32,
    pub h: u32,
    pub max_hp: u32,
    /// Höchstens so viele pro Spieler.
    #[serde(default)]
    pub max_per_player: Option<u32>,
    /// Wo das Gebäude stehen darf. Fehlt es, reicht normaler Boden.
    #[serde(default)]
    pub placement: Option<Placement>,
    /// Rohstoff-Felder, die dieser Betrieb abbaut (z. B. Wald).
    #[serde(default)]
    pub gathers: Vec<ResourceKind>,
    /// Wie weit um das Gebäude abgebaut wird (Felder vom Rand). 0 = nur unter dem Gebäude.
    #[serde(default)]
    pub gather_radius: u32,
    /// Aufgeben hinterlässt diesen Rohstoff auf der Fläche (Obstplantage → Wald).
    #[serde(default)]
    pub releases_as: Option<ResourceKind>,
    /// Wagenschuppen: der Karren fährt nur Betriebe in diesem Umkreis (Felder) an. 0 = überall.
    #[serde(default)]
    pub service_radius: u32,
    /// Mauern und Türme haben keinen Eingang; alle anderen ein Feld davor, das frei bleiben muss.
    #[serde(default)]
    pub no_entrance: bool,
    /// Bergfried und Vorratslager: hier liegt der gemeinsame Vorrat, Träger liefern hierhin.
    #[serde(default)]
    pub hub: bool,
    /// Einheiten, die zum Gebäude gehören (Träger, Karren).
    #[serde(default)]
    pub spawns: Vec<UnitCount>,
    /// Mindestens eine Stufe.
    /// Arbeiter, die der Betrieb braucht (sie wohnen in Häusern und essen).
    #[serde(default)]
    pub workers: u32,
    /// Ab dieser Stufe können eigene Einheiten oben auf dem Gebäude laufen (Wehrgang). 0 = nie.
    #[serde(default)]
    pub walkway_from_level: u32,
    /// Tor: ab dieser Stufe führt eine Treppe im Tor auf den Wehrgang. 0 = nie.
    #[serde(default)]
    pub stairs_from_level: u32,
    /// Tor: im Modus „automatisch“ schließt es, sobald fremde Einheiten so nah (Felder) sind.
    #[serde(default)]
    pub gate_radius: u32,
    /// So viele Einheiten finden oben Platz. 0 = so viele, wie draufpassen.
    #[serde(default)]
    pub garrison: u32,
    /// Höchstabstand zum eigenen Bergfried (Felder, ab dessen Mitte): die ganze Fläche
    /// muss in diesem Umkreis liegen. 0 = überall.
    #[serde(default)]
    pub build_radius: u32,
    /// Darf direkt auf eigene Mauern gesetzt werden; die Mauer darunter wird abgerissen.
    #[serde(default)]
    pub on_walls: bool,
    /// Darf auf Schrägen stehen (Mauern folgen dem Gelände).
    #[serde(default)]
    pub on_ramps: bool,
    /// Weg: das Feld bleibt begehbar, und man läuft hier so viel Prozent schneller.
    /// 0 = kein Weg.
    #[serde(default)]
    pub road_speed_percent: u32,
    /// Das erste Gebäude dieser Art je Spieler kostet nichts.
    #[serde(default)]
    pub first_free: bool,
    /// Lager nur für diese Waren (z. B. Waffenkammer). Leer = alle Waren.
    #[serde(default)]
    pub hub_goods: Vec<Good>,
    /// Platz je Ware in diesem Lager. 0 = wie bisher (`base_capacity` bzw. `storehouse_capacity`).
    #[serde(default)]
    pub hub_capacity: u32,
    /// Betriebe: so viel passt je Eingangsware ins eigene Lager. 0 = `transport.building_capacity`.
    #[serde(default)]
    pub storage_inputs: u32,
    /// Betriebe: so viel passt je Ergebnis ins eigene Lager. 0 = `transport.building_capacity`.
    #[serde(default)]
    pub storage_outputs: u32,
    pub levels: Vec<BuildingLevel>,
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
    /// So viel Prozent der Bau- und Ausbaukosten gibt es beim Abriss zurück.
    pub demolish_refund_percent: u32,
    /// So viel Prozent der Bau- und Ausbaukosten kostet das Versetzen eines Gebäudes.
    #[serde(default = "default_move_cost")]
    pub move_cost_percent: u32,
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
    /// Schaden je Treffer (für den Kampf, kommt noch).
    #[serde(default)]
    pub attack: u32,
    /// Reichweite in Feldern, 1 = Nahkampf (für den Kampf, kommt noch).
    #[serde(default)]
    pub range: f32,
    /// Sold in Talern je Zahltag (siehe `army.upkeep_seconds`).
    #[serde(default)]
    pub upkeep: u32,
    /// Fernkämpfer (Reichweite über `army.melee_range`) können diese Einheit nicht verletzen
    /// (der König: nur Nahkämpfer können ihn töten).
    #[serde(default)]
    pub ranged_immune: bool,
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
    /// Sekunden zwischen zwei Schlägen oder Schüssen einer Einheit.
    pub attack_seconds: f32,
    /// Bis zu dieser Reichweite (Felder) zählt ein Angriff als Nahkampf, darüber als Fernkampf.
    pub melee_range: f32,
    /// So viele Sekunden nach dem Tod eines Arbeiters, Trägers oder Karrens kommt Ersatz
    /// (aus einem Wohnhaus bzw. am eigenen Gebäude). 0 = nie.
    pub civilian_respawn_seconds: f32,
}

impl Default for ArmyDef {
    fn default() -> Self {
        ArmyDef { upkeep_seconds: 60.0, unpaid_speed_percent: 60, queue_max: 10, attack_seconds: 1.5, melee_range: 2.0, civilian_respawn_seconds: 30.0 }
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
}

impl Default for PopulationDef {
    fn default() -> Self {
        PopulationDef { food: vec![Good::Bread, Good::Meat, Good::Fruit], meal_seconds: 60.0, hungry_speed_percent: 50, per_transport: 1 }
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
    /// Preise je Ware. Fehlt eine Ware, kann man sie nicht handeln.
    pub prices: Vec<Price>,
}

/// Preis einer Ware am Markt: so viel zahlt man beim Kauf, so viel bekommt man beim Verkauf.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
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
    /// Bei der letzten Mahlzeit gab es nicht genug Essen.
    pub hungry: bool,
    /// So viele wurden bei der letzten Mahlzeit satt (von `working`).
    #[serde(default)]
    pub fed: u32,
    /// Sekunden (Spielzeit) bis zur nächsten Mahlzeit, Stand beim Senden.
    #[serde(default)]
    pub next_meal_in: u32,
}

impl Default for MovementDef {
    fn default() -> Self {
        MovementDef { king_speed: 1.5, worker_speed: 1.2, slope_speed_percent: default_slope_speed() }
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
    /// Karren füllen das Eingangslager auf (auch wenn der Arbeiter selbst trägt). Standard: aus.
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
}

fn default_true() -> bool {
    true
}

fn default_move_cost() -> u32 {
    25
}
