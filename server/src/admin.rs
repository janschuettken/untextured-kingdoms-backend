//! Admin: wer darf Admin-Menü, Balancing-Editor und Admin-Aktionen benutzen?
//!
//! Admin-Rechte hängen am Account (Tabelle `accounts`, Spalte `is_admin`). Der
//! Client weist sich mit seiner geheimen Spieler-Id aus (dieselbe wie in Lobby
//! und Spiel; nach dem Anmelden gehört sie zum Account), per Kopfzeile
//! `X-Player-Token` bzw. in `admin_login`.
//!
//! Admin ist, wessen Account `is_admin` hat oder dessen Name in `ADMIN_USERS`
//! steht (durch Komma getrennt, ohne Rücksicht auf Groß-/Kleinschreibung). Wer
//! in `ADMIN_USERS` steht, bekommt `is_admin` beim Start fest eingetragen; so
//! setzt man den ersten Admin. Weitere per SQL:
//! `UPDATE accounts SET is_admin = true WHERE lower(username) = lower('Name');`
//!
//! `ADMIN_OPEN=1` macht jeden zum Admin, ohne Anmeldung (Standard im
//! Debug-Build `cargo run`, damit lokal alles ohne Datenbank geht; im
//! Release-Build/Docker aus).
//!
//! - `GET  /api/admin`    `{ "enabled": bool, "open": bool }`: `open` = ohne Anmeldung
//! - `POST /api/admin`    Ist dieser Spieler Admin? (Kopfzeile `X-Player-Token`), 204 oder 401
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
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    economy::Economy,
    game::Match,
    lobby::{Game, Hub},
};

pub struct Admin {
    /// Jeder ist Admin, ohne Anmeldung.
    open: bool,
    /// Namen aus `ADMIN_USERS`, klein geschrieben.
    users: Vec<String>,
    db: Option<PgPool>,
    pub economy_file: PathBuf,
}

impl Admin {
    pub async fn from_env(db: Option<PgPool>) -> Admin {
        let open = match std::env::var("ADMIN_OPEN") {
            Ok(v) => matches!(v.trim(), "1" | "true" | "yes" | "on"),
            Err(_) => cfg!(debug_assertions),
        };
        let users = parse_users(&std::env::var("ADMIN_USERS").unwrap_or_default());
        let economy_file = std::env::var("ECONOMY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/economy.json"))
            });
        if let (Some(db), false) = (&db, users.is_empty()) {
            match sqlx::query("UPDATE accounts SET is_admin = true WHERE lower(username) = ANY($1) AND NOT is_admin")
                .bind(&users)
                .execute(db)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => info!("{} Account(s) aus ADMIN_USERS zu Admins gemacht", r.rows_affected()),
                Ok(_) => {}
                Err(e) => warn!("ADMIN_USERS eintragen: {e}"),
            }
        }
        if open {
            warn!("ADMIN_OPEN: jeder darf Admin-Menü und Balancing-Editor benutzen");
        } else if db.is_none() {
            warn!("Keine Datenbank: niemand ist Admin (Admin-Rechte hängen an Accounts)");
        }
        Admin {
            open,
            users,
            db,
            economy_file,
        }
    }

    #[cfg(test)]
    pub fn for_tests() -> Admin {
        Admin {
            open: false,
            users: Vec::new(),
            db: None,
            economy_file: PathBuf::new(),
        }
    }

    /// Jeder ist Admin, ohne Anmeldung.
    pub fn open(&self) -> bool {
        self.open
    }

    /// Gibt es überhaupt einen Weg, Admin zu sein?
    pub fn enabled(&self) -> bool {
        self.open || self.db.is_some()
    }

    /// Ist der Spieler mit diesem Token Admin?
    pub async fn check(&self, player_token: &str) -> bool {
        if self.open {
            return true;
        }
        let (Some(db), false) = (&self.db, player_token.is_empty()) else {
            return false;
        };
        let row: Option<(String, bool)> =
            sqlx::query_as("SELECT username, is_admin FROM accounts WHERE player_token = $1")
                .bind(player_token)
                .fetch_optional(db)
                .await
                .unwrap_or_else(|e| {
                    warn!("Admin prüfen: {e}");
                    None
                });
        row.is_some_and(|(name, is_admin)| is_admin || self.users.contains(&name.to_lowercase()))
    }

    async fn allows(&self, headers: &HeaderMap) -> bool {
        let token = headers
            .get("x-player-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        self.check(token).await
    }
}

fn parse_users(list: &str) -> Vec<String> {
    list.split(',')
        .map(|u| u.trim().to_lowercase())
        .filter(|u| !u.is_empty())
        .collect()
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
    Json(serde_json::json!({ "enabled": hub.admin.enabled(), "open": hub.admin.open() }))
        .into_response()
}

async fn login(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> StatusCode {
    if hub.admin.allows(&headers).await {
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
    if !hub.admin.allows(&headers).await {
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
    info!("Wirtschaftsdaten gespeichert: {}", path.display());
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
    if !hub.admin.allows(&headers).await {
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
    if !hub.admin.allows(&headers).await {
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
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let (_, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    match hub.load_world(id) {
        Ok(world) => {
            m.replace_world(world);
            info!("Spiel {id}: Spielstand geladen");
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
    if !hub.admin.allows(&headers).await {
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
            info!("Spiel {id}: neu angefangen");
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
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    hub.end_game(id);
    hub.notify();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin(open: bool) -> Admin {
        Admin {
            open,
            users: parse_users(" Jan, ritter_kunibert ,,"),
            db: None,
            economy_file: PathBuf::new(),
        }
    }

    #[test]
    fn admin_namen_lesen() {
        assert_eq!(admin(false).users, vec!["jan", "ritter_kunibert"]);
    }

    #[tokio::test]
    async fn ohne_datenbank_niemand_ausser_offen() {
        assert!(!admin(false).check("irgendein-token-1234").await);
        assert!(!admin(false).allows(&HeaderMap::new()).await);
        assert!(admin(true).check("").await, "ADMIN_OPEN: jeder ist Admin");
    }
}
