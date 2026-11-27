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
//! - `GET  /api/admin/users`  nur Admin: alle Accounts `[{ id, username, admin, fixed, maps, created, last_login, game }]`
//!   (`fixed` = steht in `ADMIN_USERS`, bleibt Admin)
//!   (Zeiten in Unix-Sekunden, `game` = Spiel, in dem der Account gerade einen Platz hat)
//!   Danach die Gäste ohne Account: `{ guest_id, username, guest: true, created, last_login, game }`
//! - `POST /api/admin/guests/{id}/impersonate`  nur Admin: als dieser Gast spielen → `{ username, token }`
//! - `GET  /api/admin/load`   nur Admin: Serverauslastung je laufendem Spiel (Dauer der Ticks gegen das Budget)
//! - `POST /api/admin/users/{id}/impersonate`  nur Admin: als dieser Nutzer anmelden → `{ username, token }`
//! - `POST /api/admin/users/{id}/admin`  nur Admin: `{ admin: bool }` Admin-Recht setzen (nicht bei sich selbst)
//! - `POST /api/admin/users/{id}/maps`   nur Admin: `{ allowed: bool }` Karteneditor freischalten (`can_edit_maps`)
//! - `POST /api/rights`  Rechte des Spielers (Kopfzeile `X-Player-Token`): `{ admin, maps }`
//! - `GET  /api/games/{id}/players`  alle Spieler des Spiels: `[{ id, name, online, buildings }]`
//! - `POST /api/games/{id}/players/{pid}/remove`  nur Admin: Reich eines (nicht verbundenen) Spielers entfernen
//! - `POST /api/games/{id}/save`     nur Admin: Spiel jetzt speichern
//! - `POST /api/games/{id}/load`     nur Admin: zuletzt gespeicherten Stand laden, alle Clients laden neu
//! - `POST /api/games/{id}/new-game` nur Admin: neu anfangen (gleiche Karte, gleiche Mitspieler)
//! - `POST /api/games/{id}/end`      nur Admin: Spiel ganz beenden, alle zurück in die Lobby
//! - `POST /api/games/{id}/duplicate` nur Admin: laufendes Spiel samt Spielstand kopieren → `{ id }`
//! - `GET  /api/games/{id}/settings`  Starteinstellungen des laufenden Spiels `{ trade_mode, shot_mode, rules }`
//! - `POST /api/games/{id}/settings`  nur Admin: dieselben nachträglich ändern (Felder optional),
//!   alle Clients laden danach neu
//! - `GET  /api/games/{id}/export`    nur Admin: aktuellen Spielstand als Datei herunterladen
//! - `POST /api/games/import`         nur Admin: Spieldatei (Rumpf) als neues Spiel starten → `{ id }`
//! - `GET  /api/economy`  die geltenden Wirtschaftsdaten als JSON
//! - `GET  /api/economy/file`  nur Admin: die geltenden Daten als Datei `economy.json` (genau im
//!   Format der Datei im Repo), zum Herunterladen und Committen
//! - `POST /api/economy/convert`  Daten prüfen und im aktuellen Aufbau zurückgeben (alte
//!   Balancing-Dateien umwandeln), ohne etwas zu speichern
//! - `PUT  /api/economy`  nur Admin: neue Daten prüfen, in die Datei schreiben und
//!   sofort für alle Spiele übernehmen (`economy_changed` an alle Clients)
//!
//! - `GET  /api/ai-personalities`  Charaktere der Computergegner (für Lobby und Editor)
//! - `GET  /api/ai-personalities/file`  nur Admin: als Datei `ai_personalities.json`
//! - `PUT  /api/ai-personalities`  nur Admin: prüfen, speichern, sofort für alle
//!   Computergegner übernehmen (auch in laufenden Spielen)
//!
//! Die Datei ist `ECONOMY_FILE`, Standard `server/data/economy.json`. Vor dem
//! Überschreiben bleibt die alte Fassung als `economy.json.bak` liegen. Charaktere
//! ebenso in `AI_PERSONALITIES_FILE`, Standard `server/data/ai_personalities.json`.

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
    ai,
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
    pub personalities_file: PathBuf,
}

impl Admin {
    /// Gast (nur Name, kein Passwort) in der Datenbank vermerken oder auffrischen.
    /// Läuft im Hintergrund; ohne Datenbank passiert nichts.
    pub fn record_guest(&self, token: &str, name: &str) {
        let Some(db) = self.db.clone() else { return };
        let (token, name) = (token.to_string(), name.to_string());
        tokio::spawn(async move {
            let r = sqlx::query(
                "INSERT INTO guests (player_token, name) VALUES ($1, $2) \
                 ON CONFLICT (player_token) DO UPDATE SET name = EXCLUDED.name, last_seen = now()",
            )
            .bind(&token)
            .bind(&name)
            .execute(&db)
            .await;
            if let Err(e) = r {
                warn!("Gast speichern: {e}");
            }
        });
    }

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
        let personalities_file = std::env::var("AI_PERSONALITIES_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/ai_personalities.json"))
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
            personalities_file,
        }
    }

    #[cfg(test)]
    pub fn for_tests() -> Admin {
        Admin {
            open: false,
            users: Vec::new(),
            db: None,
            economy_file: PathBuf::new(),
            personalities_file: PathBuf::new(),
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
        self.rights(player_token).await.admin
    }

    /// Darf der Spieler mit diesem Token Karten speichern (Admin oder `can_edit_maps`)?
    pub async fn can_edit_maps(&self, player_token: &str) -> bool {
        let r = self.rights(player_token).await;
        r.admin || r.maps
    }

    /// Rechte des Spielers mit diesem Token.
    pub async fn rights(&self, player_token: &str) -> Rights {
        if self.open {
            return Rights {
                admin: true,
                maps: true,
            };
        }
        let (Some(db), false) = (&self.db, player_token.is_empty()) else {
            return Rights::default();
        };
        let row: Option<(String, bool, bool)> = sqlx::query_as(
            "SELECT username, is_admin, can_edit_maps FROM accounts WHERE player_token = $1",
        )
        .bind(player_token)
        .fetch_optional(db)
        .await
        .unwrap_or_else(|e| {
            warn!("Rechte prüfen: {e}");
            None
        });
        match row {
            Some((name, is_admin, maps)) => Rights {
                admin: is_admin || self.users.contains(&name.to_lowercase()),
                maps,
            },
            None => Rights::default(),
        }
    }

    async fn allows(&self, headers: &HeaderMap) -> bool {
        self.check(header_token(headers)).await
    }
}

/// Was ein Spieler darf.
#[derive(Default, Clone, Copy)]
pub struct Rights {
    pub admin: bool,
    /// Karteneditor (Karten speichern).
    pub maps: bool,
}

pub fn header_token(headers: &HeaderMap) -> &str {
    headers
        .get("x-player-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
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
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/load", get(server_load))
        .route("/api/admin/users/{id}/impersonate", post(impersonate))
        .route("/api/admin/guests/{id}/impersonate", post(impersonate_guest))
        .route("/api/admin/users/{id}/admin", post(set_admin))
        .route("/api/admin/users/{id}/maps", post(set_maps))
        .route("/api/rights", post(rights))
        .route("/api/economy", get(load).put(save))
        .route("/api/economy/file", get(download))
        .route("/api/economy/convert", post(convert))
        .route("/api/ai-personalities", get(load_personalities).put(save_personalities))
        .route("/api/ai-personalities/file", get(download_personalities))
        .route("/api/games/{id}/players", get(players))
        .route("/api/games/{id}/players/{pid}/remove", post(remove_player))
        .route("/api/games/{id}/save", post(save_now))
        .route("/api/games/{id}/load", post(load_now))
        .route("/api/games/{id}/new-game", post(new_game))
        .route("/api/games/{id}/end", post(end_game))
        .route("/api/games/{id}/duplicate", post(duplicate_game))
        .route("/api/games/{id}/export", get(export_game))
        .route("/api/games/{id}/settings", get(get_settings).post(set_settings))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        // Spieldateien sind größer als die übrigen Rümpfe.
        .merge(
            Router::new()
                .route("/api/games/import", post(import_game))
                .layer(DefaultBodyLimit::max(256 * 1024 * 1024)),
        )
        .with_state(hub)
}

/// Serverauslastung: je laufendem Spiel, wie lange seine Ticks dauern (jedes Spiel hat
/// einen eigenen Thread, also verteilen sich mehrere Spiele auf die Kerne).
async fn server_load(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let games: Vec<Arc<Game>> = hub.games.lock().unwrap().values().cloned().collect();
    let list: Vec<_> = games
        .iter()
        .filter_map(|g| {
            let m = g.running.get()?;
            Some(serde_json::json!({
                "id": g.id,
                "map": m.map_name,
                "paused": m.paused(),
                "players_online": m.online_players().len(),
                "units": m.world.lock().unwrap().units.len(),
                "load": m.load.lock().unwrap().summary(),
                "history": m.load.lock().unwrap().history(),
            }))
        })
        .collect();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    Json(serde_json::json!({ "cores": cores, "tick_rate": crate::game::TICK_RATE, "games": list })).into_response()
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

// ---------- Nutzerverwaltung ----------

/// Admin geprüft und Datenbank da? Sonst die passende Antwort.
#[allow(clippy::result_large_err)]
async fn admin_db(hub: &Hub, headers: &HeaderMap) -> Result<PgPool, Response> {
    if !hub.admin.allows(headers).await {
        return Err((StatusCode::UNAUTHORIZED, "admin.denied").into_response());
    }
    hub.admin
        .db
        .clone()
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "account.disabled").into_response())
}

fn db_error(e: sqlx::Error) -> Response {
    warn!("Nutzerverwaltung: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "account.error").into_response()
}

type UserRow = (i64, String, bool, bool, i64, Option<i64>, String, Option<String>, Option<String>);

async fn list_users(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let db = match admin_db(&hub, &headers).await {
        Ok(db) => db,
        Err(r) => return r,
    };
    let rows: Vec<UserRow> = match sqlx::query_as(
        "SELECT id, username, is_admin, can_edit_maps, extract(epoch FROM created_at)::bigint, \
         extract(epoch FROM last_login)::bigint, player_token, email, avatar FROM accounts ORDER BY lower(username)",
    )
    .fetch_all(&db)
    .await
    {
        Ok(r) => r,
        Err(e) => return db_error(e),
    };
    let list: Vec<_> = rows
        .into_iter()
        .map(|(id, username, is_admin, maps, created, last_login, token, email, avatar)| {
            // Über ADMIN_USERS ist man immer Admin, egal was in der Tabelle steht.
            let fixed = hub.admin.users.contains(&username.to_lowercase());
            let game = hub.game_of(&token).map(|g| g.id);
            serde_json::json!({
                "id": id, "username": username, "admin": is_admin || fixed, "fixed": fixed, "maps": maps,
                "created": created, "last_login": last_login, "game": game, "email": email, "avatar": avatar,
            })
        })
        .collect();
    // Gäste ohne Account (wer sich später registriert, steht oben als Account).
    let guests: Vec<(i64, String, i64, i64, String)> = match sqlx::query_as(
        "SELECT g.id, g.name, extract(epoch FROM g.created_at)::bigint, extract(epoch FROM g.last_seen)::bigint, \
         g.player_token FROM guests g WHERE NOT EXISTS (SELECT 1 FROM accounts a WHERE a.player_token = g.player_token) \
         ORDER BY g.last_seen DESC",
    )
    .fetch_all(&db)
    .await
    {
        Ok(r) => r,
        Err(e) => return db_error(e),
    };
    let mut list = list;
    list.extend(guests.into_iter().map(|(id, name, created, seen, token)| {
        let game = hub.game_of(&token).map(|g| g.id);
        serde_json::json!({
            "id": null, "guest_id": id, "guest": true, "username": name, "admin": false, "fixed": false,
            "maps": false, "created": created, "last_login": seen, "game": game,
        })
    }));
    Json(list).into_response()
}

async fn impersonate_guest(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let db = match admin_db(&hub, &headers).await {
        Ok(db) => db,
        Err(r) => return r,
    };
    let row: Option<(String, String)> =
        match sqlx::query_as("SELECT name, player_token FROM guests WHERE id = $1").bind(id).fetch_optional(&db).await {
            Ok(r) => r,
            Err(e) => return db_error(e),
        };
    let Some((name, token)) = row else {
        return (StatusCode::NOT_FOUND, "admin.no_user").into_response();
    };
    info!("Admin spielt als Gast {name}");
    Json(serde_json::json!({ "username": name, "token": token })).into_response()
}

async fn impersonate(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let db = match admin_db(&hub, &headers).await {
        Ok(db) => db,
        Err(r) => return r,
    };
    let row: Option<(String, String)> =
        match sqlx::query_as("SELECT username, player_token FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(&db)
            .await
        {
            Ok(r) => r,
            Err(e) => return db_error(e),
        };
    let Some((username, token)) = row else {
        return (StatusCode::NOT_FOUND, "admin.no_user").into_response();
    };
    info!("Admin meldet sich als {username} an");
    Json(serde_json::json!({ "username": username, "token": token })).into_response()
}

#[derive(serde::Deserialize)]
struct SetAdmin {
    admin: bool,
}

async fn set_admin(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<SetAdmin>,
) -> Response {
    let db = match admin_db(&hub, &headers).await {
        Ok(db) => db,
        Err(r) => return r,
    };
    // Sich selbst nicht entmachten, sonst sperrt man sich aus.
    let me: Option<(i64,)> = sqlx::query_as("SELECT id FROM accounts WHERE player_token = $1")
        .bind(header_token(&headers))
        .fetch_optional(&db)
        .await
        .unwrap_or(None);
    if me.is_some_and(|(mine,)| mine == id) {
        return (StatusCode::CONFLICT, "admin.not_self").into_response();
    }
    match sqlx::query("UPDATE accounts SET is_admin = $1 WHERE id = $2")
        .bind(body.admin)
        .bind(id)
        .execute(&db)
        .await
    {
        Ok(r) if r.rows_affected() == 0 => (StatusCode::NOT_FOUND, "admin.no_user").into_response(),
        Ok(_) => {
            info!("Account {id}: Admin = {}", body.admin);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => db_error(e),
    }
}

async fn download(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let text = hub.economy.lock().unwrap().to_json();
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"economy.json\"",
            ),
        ],
        text,
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct SetMaps {
    allowed: bool,
}

async fn set_maps(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<SetMaps>,
) -> Response {
    let db = match admin_db(&hub, &headers).await {
        Ok(db) => db,
        Err(r) => return r,
    };
    match sqlx::query("UPDATE accounts SET can_edit_maps = $1 WHERE id = $2")
        .bind(body.allowed)
        .bind(id)
        .execute(&db)
        .await
    {
        Ok(r) if r.rows_affected() == 0 => (StatusCode::NOT_FOUND, "admin.no_user").into_response(),
        Ok(_) => {
            info!("Account {id}: Karteneditor = {}", body.allowed);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => db_error(e),
    }
}

async fn rights(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let r = hub.admin.rights(header_token(&headers)).await;
    Json(serde_json::json!({ "admin": r.admin, "maps": r.admin || r.maps })).into_response()
}

async fn load(State(hub): State<Arc<Hub>>) -> Response {
    // Vollständig (mit allen Standardwerten), damit der Editor jedes Feld vorfindet.
    Json(hub.economy.lock().unwrap().def.clone()).into_response()
}

/// Prüfen und umwandeln (alte Dateien in den aktuellen Aufbau), nichts speichern.
async fn convert(body: String) -> Response {
    match Economy::parse(&body) {
        Ok(e) => Json(e.def).into_response(),
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    }
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

async fn load_personalities(State(hub): State<Arc<Hub>>) -> Response {
    Json(hub.personalities.lock().unwrap().clone()).into_response()
}

async fn download_personalities(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let text = ai::personality::to_json(&hub.personalities.lock().unwrap());
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"ai_personalities.json\""),
        ],
        text,
    )
        .into_response()
}

async fn save_personalities(State(hub): State<Arc<Hub>>, headers: HeaderMap, body: String) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    let all = match ai::personality::parse(&body) {
        Ok(a) => a,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    };
    let path = &hub.admin.personalities_file;
    if let Err(e) = write_with_backup(path, &ai::personality::to_json(&all)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", path.display())).into_response();
    }
    *hub.personalities.lock().unwrap() = all.clone();
    let games: Vec<_> = hub.games.lock().unwrap().values().filter_map(|g| g.running.get().cloned()).collect();
    for m in games {
        let world = m.world.lock().unwrap();
        for c in m.computers.lock().unwrap().iter_mut() {
            c.reconfigure(&all, &world.economy);
        }
    }
    info!("Charaktere der Computergegner gespeichert: {}", path.display());
    StatusCode::NO_CONTENT.into_response()
}

/// Datei ersetzen, alte Fassung als `.json.bak` behalten.
fn write_with_backup(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        std::fs::copy(path, path.with_extension("json.bak"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
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

async fn duplicate_game(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    if let Err(r) = running(&hub, id) {
        return r;
    }
    match tokio::task::spawn_blocking(move || hub.duplicate(id)).await {
        Ok(Ok(new_id)) => Json(serde_json::json!({ "id": new_id })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct GameSettings {
    #[serde(default)]
    trade_mode: Option<protocol::TradeMode>,
    #[serde(default)]
    shot_mode: Option<protocol::ShotMode>,
    #[serde(default)]
    rules: Option<protocol::GameRules>,
}

async fn get_settings(State(hub): State<Arc<Hub>>, Path(id): Path<u32>) -> Response {
    let (_, m) = match running(&hub, id) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let world = m.world.lock().unwrap();
    Json(serde_json::json!({ "trade_mode": world.trade_mode(), "shot_mode": world.shot_mode(), "rules": world.rules() })).into_response()
}

async fn set_settings(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
    Json(body): Json<GameSettings>,
) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    match hub.change_settings(id, body.trade_mode, body.shot_mode, body.rules) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn export_game(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    if let Err(r) = running(&hub, id) {
        return r;
    }
    match tokio::task::spawn_blocking(move || hub.export(id)).await {
        Ok(Ok(text)) => (
            [
                (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"spiel-{id}.json\""),
                ),
            ],
            text,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn import_game(State(hub): State<Arc<Hub>>, headers: HeaderMap, body: String) -> Response {
    if !hub.admin.allows(&headers).await {
        return (StatusCode::UNAUTHORIZED, "admin.denied").into_response();
    }
    match tokio::task::spawn_blocking(move || hub.import(&body)).await {
        Ok(Ok(id)) => Json(serde_json::json!({ "id": id })).into_response(),
        Ok(Err(e)) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
            personalities_file: PathBuf::new(),
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
