//! Gemeinsame Netzwerk-Nachrichten zwischen Server und Client.
//!
//! Die Rust-Typen hier sind die einzige Quelle der Wahrheit. Die passenden
//! TypeScript-Typen für den Client werden mit `cargo test -p protocol`
//! nach `protocol/bindings/` generiert. Im Frontend-Repo holt
//! `npm run sync-protocol` sie nach `src/protocol/`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Nachrichten vom Client an den Server.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ClientMsg {
    Hello { name: String },
    Ping { client_time: f64 },
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
    },
    Pong {
        client_time: f64,
    },
}

/// Karte als Gitter mit diskreten Höhenstufen (0 = Tal, höher = Anhöhe).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MapInfo {
    pub width: u32,
    pub height: u32,
    /// Zeilenweise, Länge = width * height.
    pub levels: Vec<u8>,
    pub max_level: u8,
}
