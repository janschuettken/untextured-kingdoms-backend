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
}

/// Nachrichten vom Server an den Client.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerMsg {
    Welcome {
        player_id: u32,
        tick_rate: u32,
        map: MapInfo,
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
