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
    },
    /// Eigenes Gebäude abreißen. Ein Teil der Baukosten kommt zurück.
    DemolishBuilding { building_id: u32 },
    /// Eigenes Gebäude auf die nächste Stufe ausbauen.
    UpgradeBuilding { building_id: u32 },
    /// Einstellungen eines eigenen Gebäudes ändern (was es herstellt usw.).
    ConfigureBuilding { building_id: u32, settings: BuildingSettings },
    /// Obstplantage aufgeben: ihre Bäume werden zu Wald, den man abholzen kann.
    ReleaseTrees { building_id: u32 },
    /// Als Admin anmelden (Token aus der Server-Umgebung `ADMIN_TOKEN`).
    AdminLogin { token: String },
    /// Nur Admin: sich selbst Waren ins Lager legen.
    AdminGive { goods: Vec<GoodAmount> },
    /// Nur Admin: Spieltempo für alle in Prozent (100 = normal, 25 bis 800).
    AdminGameSpeed { percent: u32 },
    /// Sichtbereich des Clients in Feldern (Rechteck, Ecken eingeschlossen).
    /// Der Server schickt Einheiten nur für diesen Bereich (plus Rand) und
    /// immer die eigenen. Ohne diese Nachricht sieht der Client alles.
    View { x0: f32, y0: f32, x1: f32, y1: f32 },
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
/// Client als Text von `GET /api/maps/current` (Format wie `server/maps/*.txt`,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UnitKind {
    King,
    /// Träger: trägt wenig, wohnt im Bergfried oder Vorratslager.
    Carrier,
    /// Karren aus dem Wagenschuppen: fährt viel auf einmal.
    Cart,
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
    /// Höchstmenge je Ware (wächst mit Vorratslagern).
    pub capacity: u32,
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
    Tower,
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
}

/// Gruppe im Baumenü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BuildingCategory {
    Fortification,
    Storage,
    Raw,
    Workshop,
    Food,
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
}

/// Die ganze Wirtschaft als Daten (einzige Quelle: `server/data/economy.json`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EconomyDef {
    pub goods: Vec<Good>,
    pub buildings: Vec<BuildingDef>,
    pub recipes: Vec<RecipeDef>,
    pub start_stock: Vec<GoodAmount>,
    /// Lagerplatz je Ware ohne Vorratslager.
    pub base_capacity: u32,
    /// Zusätzlicher Platz je Ware pro Vorratslager.
    pub storehouse_capacity: u32,
    /// So viel Prozent der Bau- und Ausbaukosten gibt es beim Abriss zurück.
    pub demolish_refund_percent: u32,
    /// Wie viel ein Feld hergibt, bevor es erschöpft ist (Wald, Stein, Eisen).
    pub resource_amounts: Vec<ResourceAmount>,
    /// So viel schneller arbeiten Höfe auf besonders fruchtbarem Boden (anteilig).
    pub fertile_bonus_percent: u32,
    pub transport: TransportDef,
    #[serde(default)]
    pub movement: MovementDef,
}

/// Lauftempo der Einheiten, die nicht transportieren (Felder pro Sekunde).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MovementDef {
    pub king_speed: f32,
}

impl Default for MovementDef {
    fn default() -> Self {
        MovementDef { king_speed: 1.5 }
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
    /// Nur Lager: welche Waren hier eingelagert werden. `None` = alle.
    #[serde(default)]
    pub stores: Option<Vec<Good>>,
    /// Mindestbestand im Vorrat: von diesen Waren bekommt der Betrieb nur, was
    /// darüber liegt (z. B. Holz fürs Bauen behalten).
    #[serde(default)]
    pub keep_in_stock: Vec<GoodAmount>,
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
}
