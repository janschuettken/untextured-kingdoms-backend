//! Admin: Token-Prüfung und Balancing-Schnittstelle.
//!
//! Der Admin-Token kommt aus der Umgebungsvariable `ADMIN_TOKEN`. Ist sie
//! nicht gesetzt (vorerst der Normalfall), ist jeder Admin, ganz ohne Anmeldung.
//! Ist sie gesetzt, muss man den Token eingeben.
//!
//! - `GET  /api/admin`    `{ "enabled": true, "open": bool }`: `open` = ohne Anmeldung
//! - `POST /api/admin`    Token prüfen (Kopfzeile `X-Admin-Token`), 204 oder 401
//! - `GET  /api/players`  alle Spieler: `[{ id, online, buildings }]`
//! - `POST /api/players/{id}/remove`  nur Admin: Spieler (nicht verbunden) mit allem entfernen
//! - `POST /api/save`     nur Admin: Spielstand jetzt speichern (`SAVE_FILE`)
//! - `POST /api/load`     nur Admin: gespeicherten Spielstand laden, alle Clients laden neu
//! - `POST /api/new-game` nur Admin: neues Spiel auf derselben Karte, alle Clients laden neu
//! - `GET  /api/economy`  die geltenden Wirtschaftsdaten als JSON
//! - `PUT  /api/economy`  nur Admin: neue Daten prüfen, in die Datei schreiben und
//!   sofort für alle übernehmen (`economy_changed` an alle Clients)
//!
//! Die Datei ist `ECONOMY_FILE`, Standard `server/data/economy.json`. Vor dem
//! Überschreiben bleibt die alte Fassung als `economy.json.bak` liegen.

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use protocol::ServerMsg;

use crate::{economy::Economy, AppState};

pub struct Admin {
    token: Option<String>,
    pub economy_file: PathBuf,
}

impl Admin {
    pub fn from_env() -> Admin {
        let token = std::env::var("ADMIN_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let economy_file = std::env::var("ECONOMY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/economy.json")));
        Admin { token, economy_file }
    }

    /// Ohne `ADMIN_TOKEN` braucht es keine Anmeldung.
    pub fn open(&self) -> bool {
        self.token.is_none()
    }

    /// Stimmt der Token? Vergleich ohne früh abzubrechen, damit die Laufzeit nichts verrät.
    pub fn check(&self, given: &str) -> bool {
        let Some(token) = &self.token else { return true };
        let (a, b) = (token.as_bytes(), given.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (i, &x) in a.iter().enumerate() {
            diff |= (x ^ b.get(i).copied().unwrap_or(0)) as usize;
        }
        diff == 0
    }

    fn allows(&self, headers: &HeaderMap) -> bool {
        self.open() || headers.get("x-admin-token").and_then(|v| v.to_str().ok()).is_some_and(|t| self.check(t))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/admin", get(status).post(login))
        .route("/api/economy", get(load).put(save))
        .route("/api/players", get(players))
        .route("/api/players/{id}/remove", post(remove_player))
        .route("/api/save", post(save_now))
        .route("/api/load", post(load_now))
        .route("/api/new-game", post(new_game))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

async fn status(State(state): State<Arc<AppState>>) -> Response {
    Json(serde_json::json!({ "enabled": true, "open": state.admin.open() })).into_response()
}

async fn login(State(state): State<Arc<AppState>>, headers: HeaderMap) -> StatusCode {
    if state.admin.allows(&headers) { StatusCode::NO_CONTENT } else { StatusCode::UNAUTHORIZED }
}

async fn load(State(state): State<Arc<AppState>>) -> Response {
    // Vollständig (mit allen Standardwerten), damit der Editor jedes Feld vorfindet.
    Json(state.world.lock().unwrap().economy.def.clone()).into_response()
}

async fn save(State(state): State<Arc<AppState>>, headers: HeaderMap, body: String) -> Response {
    if !state.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let economy = match Economy::parse(&body) {
        Ok(e) => e,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    };
    let path = &state.admin.economy_file;
    let text = economy.to_json();
    let written = (|| {
        if path.exists() {
            std::fs::copy(path, path.with_extension("json.bak"))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", path.display())).into_response();
    }
    let mut world = state.world.lock().unwrap();
    world.set_economy(economy);
    // Unter dem Lock senden, damit es vor den nächsten Änderungen ankommt.
    let _ = state.ticks.send(ServerMsg::EconomyChanged { economy: world.economy.def.clone() });
    tracing::info!("Wirtschaftsdaten gespeichert: {}", path.display());
    StatusCode::NO_CONTENT.into_response()
}

async fn players(State(state): State<Arc<AppState>>) -> Response {
    let world = state.world.lock().unwrap();
    let online: Vec<u32> = state.clients.lock().unwrap().keys().copied().collect();
    let list: Vec<_> = world
        .players()
        .into_iter()
        .map(|id| {
            let buildings = world.buildings.iter().filter(|b| b.owner == id).count();
            serde_json::json!({ "id": id, "online": online.contains(&id), "buildings": buildings })
        })
        .collect();
    Json(list).into_response()
}

async fn remove_player(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(id): Path<u32>) -> Response {
    if !state.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let mut world = state.world.lock().unwrap();
    if state.clients.lock().unwrap().contains_key(&id) {
        return (StatusCode::CONFLICT, "admin.player_online").into_response();
    }
    for building_id in world.remove_player(id) {
        let _ = state.ticks.send(ServerMsg::BuildingRemoved { building_id });
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn save_now(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !state.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    match tokio::task::spawn_blocking(move || crate::save_game(&state)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Neue Welt einsetzen und alle Clients neu laden lassen.
fn replace_world(state: &AppState, world: crate::World) {
    let mut w = state.world.lock().unwrap();
    let next = world.players().last().map_or(1, |p| p + 1);
    *w = world;
    state.next_player_id.fetch_max(next, std::sync::atomic::Ordering::Relaxed);
    let _ = state.ticks.send(ServerMsg::Reload);
}

async fn load_now(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !state.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let economy = state.world.lock().unwrap().economy.clone();
    match crate::load_game(&state.save_file, &state.map_name, &state.map_text, economy) {
        Ok(world) => {
            replace_world(&state, world);
            tracing::info!("Spielstand geladen");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    }
}

async fn new_game(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !state.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let economy = state.world.lock().unwrap().economy.clone();
    match crate::map::GameMap::parse(&state.map_text) {
        Ok(map) => {
            replace_world(&state, crate::World::new(map, economy));
            tracing::info!("Neues Spiel gestartet");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_wird_genau_verglichen() {
        let admin = Admin { token: Some("geheim".into()), economy_file: PathBuf::new() };
        assert!(admin.check("geheim"));
        assert!(!admin.check("geheim2"));
        assert!(!admin.check("gehei"));
        assert!(!admin.check(""));
        let none = Admin { token: None, economy_file: PathBuf::new() };
        assert!(none.check(""), "ohne ADMIN_TOKEN ist jeder Admin");
        assert!(none.allows(&HeaderMap::new()));
    }
}
