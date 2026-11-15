//! Spieler-Accounts in PostgreSQL (`DATABASE_URL`, z. B.
//! `postgres://uk:passwort@localhost:5432/untextured_kingdoms`).
//!
//! Ohne Account spielt man mit einer geheimen Id (Token), die der Browser
//! selbst erzeugt und speichert. Ein Account bindet diesen Token an Name und
//! Passwort: Wer sich (auch auf einem anderen Gerät) anmeldet, bekommt den
//! Token zurück und ist wieder derselbe Spieler.
//!
//! - `GET  /api/account`           `{ "enabled": bool }` (ohne `DATABASE_URL` aus)
//! - `POST /api/account/register`  `{ username, password, token }` → `{ username, token }`
//! - `POST /api/account/login`     `{ username, password }` → `{ username, token }`
//! - `POST /api/account/me`        `{ token }` → `{ username, admin }` oder `{ username: null }`
//!
//! Fehler kommen als `{ "error": "account.…" }` (Schlüssel, der Client übersetzt).
//! Passwörter werden mit Argon2 gehasht. Die Tabellen legt der Server beim
//! Start selbst an (`server/migrations/`).

use std::sync::Arc;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::{info, warn};

#[derive(Clone)]
pub struct Accounts {
    db: Option<PgPool>,
}

impl Accounts {
    /// Mit der Datenbank aus `DATABASE_URL` verbinden und Tabellen anlegen.
    /// Klappt das nicht, läuft der Server ohne Accounts weiter.
    pub async fn from_env() -> Accounts {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            info!("Keine DATABASE_URL: Accounts sind aus, gespielt wird mit Browser-Id");
            return Accounts { db: None };
        };
        let pool = match PgPoolOptions::new().max_connections(5).acquire_timeout(std::time::Duration::from_secs(5)).connect(&url).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Datenbank nicht erreichbar ({e}): Accounts sind aus");
                return Accounts { db: None };
            }
        };
        if let Err(e) = sqlx::migrate!("./migrations").run(&pool).await {
            warn!("Datenbank-Tabellen nicht angelegt ({e}): Accounts sind aus");
            return Accounts { db: None };
        }
        info!("Datenbank verbunden, Accounts sind an");
        Accounts { db: Some(pool) }
    }
}

impl Accounts {
    /// Die Datenbank, falls verbunden (für die Admin-Prüfung).
    pub fn pool(&self) -> Option<PgPool> {
        self.db.clone()
    }
}

pub fn router(accounts: Accounts) -> Router {
    Router::new()
        .route("/api/account", get(status))
        .route("/api/account/register", post(register))
        .route("/api/account/login", post(login))
        .route("/api/account/me", post(me))
        .with_state(Arc::new(accounts))
}

fn fail(status: StatusCode, key: &str) -> Response {
    (status, Json(serde_json::json!({ "error": key }))).into_response()
}

fn valid_username(name: &str) -> bool {
    (3..=24).contains(&name.chars().count()) && name.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c))
}

/// Gleiche Regeln wie für die Browser-Id in der Lobby.
fn valid_token(token: &str) -> bool {
    (16..=64).contains(&token.len()) && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

async fn status(State(acc): State<Arc<Accounts>>) -> Response {
    Json(serde_json::json!({ "enabled": acc.db.is_some() })).into_response()
}

#[derive(Deserialize)]
struct RegisterBody {
    username: String,
    password: String,
    token: String,
}

async fn register(State(acc): State<Arc<Accounts>>, Json(b): Json<RegisterBody>) -> Response {
    let Some(db) = &acc.db else { return fail(StatusCode::SERVICE_UNAVAILABLE, "account.disabled") };
    let username = b.username.trim().to_string();
    if !valid_username(&username) {
        return fail(StatusCode::BAD_REQUEST, "account.invalid_name");
    }
    if b.password.chars().count() < 8 || b.password.len() > 200 {
        return fail(StatusCode::BAD_REQUEST, "account.password_short");
    }
    if !valid_token(&b.token) {
        return fail(StatusCode::BAD_REQUEST, "account.invalid_token");
    }
    let password = b.password;
    let hash = tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string())
    })
    .await;
    let Ok(Ok(hash)) = hash else { return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error") };
    let result = sqlx::query("INSERT INTO accounts (username, password_hash, player_token, last_login) VALUES ($1, $2, $3, now())")
        .bind(&username)
        .bind(&hash)
        .bind(&b.token)
        .execute(db)
        .await;
    match result {
        Ok(_) => {
            info!("Account {username} angelegt");
            Json(serde_json::json!({ "username": username, "token": b.token })).into_response()
        }
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            let key = if e.constraint() == Some("accounts_player_token_key") { "account.token_taken" } else { "account.name_taken" };
            fail(StatusCode::CONFLICT, key)
        }
        Err(e) => {
            warn!("Account anlegen: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error")
        }
    }
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(State(acc): State<Arc<Accounts>>, Json(b): Json<LoginBody>) -> Response {
    let Some(db) = &acc.db else { return fail(StatusCode::SERVICE_UNAVAILABLE, "account.disabled") };
    let row: Result<Option<(i64, String, String, String)>, _> =
        sqlx::query_as("SELECT id, username, password_hash, player_token FROM accounts WHERE lower(username) = lower($1)")
            .bind(b.username.trim())
            .fetch_optional(db)
            .await;
    let row = match row {
        Ok(r) => r,
        Err(e) => {
            warn!("Anmelden: {e}");
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error");
        }
    };
    // Auch bei unbekanntem Namen hashen, damit die Antwortzeit nichts verrät.
    let (id, username, hash, token) = row.unwrap_or((0, String::new(), String::new(), String::new()));
    let password = b.password;
    let ok = tokio::task::spawn_blocking(move || match PasswordHash::new(&hash) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => {
            let salt = SaltString::generate(&mut OsRng);
            let _ = Argon2::default().hash_password(password.as_bytes(), &salt);
            false
        }
    })
    .await
    .unwrap_or(false);
    if !ok {
        return fail(StatusCode::UNAUTHORIZED, "account.wrong_login");
    }
    let _ = sqlx::query("UPDATE accounts SET last_login = now() WHERE id = $1").bind(id).execute(db).await;
    Json(serde_json::json!({ "username": username, "token": token })).into_response()
}

#[derive(Deserialize)]
struct MeBody {
    token: String,
}

async fn me(State(acc): State<Arc<Accounts>>, Json(b): Json<MeBody>) -> Response {
    let Some(db) = &acc.db else { return Json(serde_json::json!({ "username": null })).into_response() };
    let name: Option<(String, bool)> = sqlx::query_as("SELECT username, is_admin FROM accounts WHERE player_token = $1")
        .bind(&b.token)
        .fetch_optional(db)
        .await
        .unwrap_or(None);
    Json(serde_json::json!({ "username": name.as_ref().map(|n| &n.0), "admin": name.is_some_and(|n| n.1) })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namen_pruefen() {
        assert!(valid_username("Jan"));
        assert!(valid_username("ritter_kunibert-2"));
        assert!(!valid_username("ab"));
        assert!(!valid_username("mit leerzeichen"));
        assert!(!valid_username(&"x".repeat(25)));
    }
}
