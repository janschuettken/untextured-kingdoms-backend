//! Admin: Token-Prüfung und Balancing-Schnittstelle.
//!
//! Der Admin-Token kommt aus der Umgebungsvariable `ADMIN_TOKEN`. Ist sie
//! nicht gesetzt (vorerst der Normalfall), ist jeder Admin, ganz ohne Anmeldung.
//! Ist sie gesetzt, muss man den Token eingeben.
//!
//! - `GET  /api/admin`    `{ "enabled": true, "open": bool }`: `open` = ohne Anmeldung
//! - `POST /api/admin`    Token prüfen (Kopfzeile `X-Admin-Token`), 204 oder 401
//! - `GET  /api/games/{id}/players`  alle Spieler des Spiels: `[{ id, name, online, buildings }]`
//! - `POST /api/games/{id}/players/{pid}/remove`  nur Admin: Reich eines (nicht verbundenen) Spielers entfernen
//! - `POST /api/games/{id}/save`     nur Admin: Spiel jetzt speichern
//! - `POST /api/games/{id}/load`     nur Admin: zuletzt gespeicherten Stand laden, alle Clients laden neu
//! - `POST /api/games/{id}/new-game` nur Admin: neu anfangen (gleiche Karte, gleiche Mitspieler)
//! - `POST /api/games/{id}/end`      nur Admin: Spiel ganz beenden, alle zurück in die Lobby
//! - `GET  /api/economy`  die geltenden Wirtschaftsdaten als JSON
//! - `PUT  /api/economy`  nur Admin: neue Daten prüfen, in die Datei schreiben und
//!   sofort für alle Spiele übernehmen (`economy_changed` an alle Clients)
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

use crate::{
    economy::Economy,
    game::Match,
    lobby::{Game, Hub},
};

pub struct Admin {
    token: Option<String>,
    pub economy_file: PathBuf,
}

impl Admin {
    pub fn from_env() -> Admin {
        let token = std::env::var("ADMIN_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let economy_file = std::env::var("ECONOMY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/economy.json"))
            });
        Admin {
            token,
            economy_file,
        }
    }

    /// Ohne `ADMIN_TOKEN` braucht es keine Anmeldung.
    pub fn open(&self) -> bool {
        self.token.is_none()
    }

    /// Stimmt der Token? Vergleich ohne früh abzubrechen, damit die Laufzeit nichts verrät.
    pub fn check(&self, given: &str) -> bool {
        let Some(token) = &self.token else {
            return true;
        };
        let (a, b) = (token.as_bytes(), given.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (i, &x) in a.iter().enumerate() {
            diff |= (x ^ b.get(i).copied().unwrap_or(0)) as usize;
        }
        diff == 0
    }

    fn allows(&self, headers: &HeaderMap) -> bool {
        self.open()
            || headers
                .get("x-admin-token")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| self.check(t))
    }
}

pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/api/admin", get(status).post(login))
        .route("/api/economy", get(load).put(save))
        .route("/api/games/{id}/players", get(players))
        .route("/api/games/{id}/players/{pid}/remove", post(remove_player))
        .route("/api/games/{id}/save", post(save_now))
        .route("/api/games/{id}/load", post(load_now))
        .route("/api/games/{id}/new-game", post(new_game))
        .route("/api/games/{id}/end", post(end_game))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(hub)
}

async fn status(State(hub): State<Arc<Hub>>) -> Response {
    Json(serde_json::json!({ "enabled": true, "open": hub.admin.open() })).into_response()
}

async fn login(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> StatusCode {
    if hub.admin.allows(&headers) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn load(State(hub): State<Arc<Hub>>) -> Response {
    // Vollständig (mit allen Standardwerten), damit der Editor jedes Feld vorfindet.
    Json(hub.economy.lock().unwrap().def.clone()).into_response()
}

async fn save(State(hub): State<Arc<Hub>>, headers: HeaderMap, body: String) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let economy = match Economy::parse(&body) {
        Ok(e) => e,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    };
    let path = &hub.admin.economy_file;
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
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", path.display()),
        )
            .into_response();
    }
    *hub.economy.lock().unwrap() = economy.clone();
    let games: Vec<_> = hub
        .games
        .lock()
        .unwrap()
        .values()
        .filter_map(|g| g.running.get().cloned())
        .collect();
    for m in games {
        let mut world = m.world.lock().unwrap();
        world.set_economy(economy.clone());
        // Unter dem Lock senden, damit es vor den nächsten Änderungen ankommt.
        let _ = m.ticks.send(ServerMsg::EconomyChanged {
            economy: world.economy.def.clone(),
        });
    }
    tracing::info!("Wirtschaftsdaten gespeichert: {}", path.display());
    StatusCode::NO_CONTENT.into_response()
}

/// Das laufende Spiel zur Id, sonst 404.
#[allow(clippy::result_large_err)]
fn running(hub: &Hub, id: u32) -> Result<(Arc<Game>, Arc<Match>), Response> {
    hub.game(id)
        .and_then(|g| Some((g.clone(), g.running.get()?.clone())))
        .ok_or_else(|| (StatusCode::NOT_FOUND, "admin.no_game").into_response())
}

async fn players(State(hub): State<Arc<Hub>>, Path(id): Path<u32>) -> Response {
    let (game, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let names: Vec<Option<String>> = game
        .meta
        .lock()
        .unwrap()
        .slots
        .iter()
        .map(|s| s.as_ref().map(|m| m.name.clone()))
        .collect();
    let world = m.world.lock().unwrap();
    let online = m.online_players();
    let list: Vec<_> = world
        .players()
        .into_iter()
        .map(|pid| {
            let buildings = world.buildings.iter().filter(|b| b.owner == pid).count();
            let name = names.get(pid as usize - 1).cloned().flatten();
            serde_json::json!({ "id": pid, "name": name, "online": online.contains(&pid), "buildings": buildings })
        })
        .collect();
    Json(list).into_response()
}

async fn remove_player(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path((id, pid)): Path<(u32, u32)>,
) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let (_, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let mut world = m.world.lock().unwrap();
    if m.is_online(pid) {
        return (StatusCode::CONFLICT, "admin.player_online").into_response();
    }
    for building_id in world.remove_player(pid) {
        let _ = m.ticks.send(ServerMsg::BuildingRemoved { building_id });
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn save_now(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let (game, _) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || hub.save_game(&game)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn load_now(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let (_, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    match hub.load_world(id) {
        Ok(world) => {
            m.replace_world(world);
            tracing::info!("Spiel {id}: Spielstand geladen");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    }
}

async fn new_game(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let (_, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    match crate::map::GameMap::parse(&m.map_text) {
        Ok(map) => {
            // Die Mitspieler bekommen ihren König beim Verbinden wieder aufs eigene Startfeld.
            m.replace_world(crate::world::World::new(
                map,
                hub.economy.lock().unwrap().clone(),
            ));
            tracing::info!("Spiel {id}: neu angefangen");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn end_game(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    hub.end_game(id);
    hub.notify();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_wird_genau_verglichen() {
        let admin = Admin {
            token: Some("geheim".into()),
            economy_file: PathBuf::new(),
        };
        assert!(admin.check("geheim"));
        assert!(!admin.check("geheim2"));
        assert!(!admin.check("gehei"));
        assert!(!admin.check(""));
        let none = Admin {
            token: None,
            economy_file: PathBuf::new(),
        };
        assert!(none.check(""), "ohne ADMIN_TOKEN ist jeder Admin");
        assert!(none.allows(&HeaderMap::new()));
    }
}
