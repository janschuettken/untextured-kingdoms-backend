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
    PlaceBuilding { kind: BuildingKind, x: u32, y: u32 },
    /// Eigenes Gebäude abreißen. Ein Teil der Baukosten kommt zurück.
    DemolishBuilding { building_id: u32 },
    /// Eigenes Gebäude auf die nächste Stufe ausbauen.
    UpgradeBuilding { building_id: u32 },
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
        map: MapInfo,
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
        /// Alle Einheiten mit ihrem aktuellen Zustand.
        units: Vec<UnitState>,
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
    /// Eigener Lagerbestand hat sich geändert (geht nur an diesen Spieler).
    Stock {
        stock: StockState,
    },
    /// Ein Befehl dieses Clients wurde abgelehnt (z. B. kein Platz zum Bauen).
    Rejected {
        text: Text,
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

/// Karte als Gitter aus Feldern.
///
/// Jedes Feld hat vier Eckhöhen. Flache Felder haben vier gleiche Ecken,
/// Rampen steigen gleichmäßig an. Wo zwei Nachbarfelder an ihrer gemeinsamen
/// Kante nicht dieselben Höhen haben, ist dort eine Klippe (Wand).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapInfo {
    pub width: u32,
    pub height: u32,
    pub max_level: u8,
    /// Wie viele Höhenschritte eine Stufe hat (Eckhöhen sind in Schritten).
    pub steps_per_level: u16,
    /// Pro Feld zeilenweise, Länge = width * height.
    pub tiles: Vec<TileKind>,
    /// Ressource pro Feld (Wald, Stein, ...). Wald, Stein und Eisen blockieren das Laufen.
    pub resources: Vec<ResourceKind>,
    /// Logische Höhenstufe pro Feld (bei Rampen die untere Stufe).
    pub levels: Vec<u8>,
    /// Vier Eckhöhen pro Feld in Schritten, Reihenfolge NW, NO, SO, SW
    /// (also (x,y), (x+1,y), (x+1,y+1), (x,y+1)). Länge = 4 * width * height.
    pub corners: Vec<u16>,
    /// Navigationsgitter: pro Feld ein Bit je Richtung, in die man laufen kann.
    /// Bit 0 = Ost (+x), dann im Uhrzeigersinn: 1 = SO, 2 = Süd (+y), 3 = SW,
    /// 4 = West, 5 = NW, 6 = Nord, 7 = NO. 0 heißt: Feld ist nicht begehbar
    /// oder komplett eingeschlossen.
    pub nav: Vec<u8>,
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
    /// Die Karte, so gut sie sich lesen ließ. Fehlt nur, wenn das Format
    /// grundsätzlich kaputt ist (z. B. Zeilen unterschiedlich lang).
    pub map: Option<MapInfo>,
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
    PigFarm,
    CattleFarm,
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
    /// Rohstoff-Felder, an denen dieser Betrieb arbeitet (z. B. Wald).
    #[serde(default)]
    pub gathers: Vec<ResourceKind>,
    /// Mindestens eine Stufe.
    pub levels: Vec<BuildingLevel>,
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
}
