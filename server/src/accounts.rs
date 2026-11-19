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
//! - `POST /api/account/me`        `{ token }` → `{ username, admin, email, avatar }` oder `{ username: null }`
//! - `POST /api/account/profile`   `{ token, username?, email?, avatar?, current_password?, new_password? }`
//!   Profil ändern (nur was mitkommt; `""` bei E-Mail/Bild löscht). Neues Passwort braucht
//!   das alte. → wie `me`
//!
//! - `POST /api/account/forgot`    `{ login, lang? }` (Name oder E-Mail) → 204, immer (verrät nicht,
//!   ob es den Account gibt). Hat er eine E-Mail, geht ein Link `…/reset.html?token=…` hin (1 Stunde gültig).
//! - `POST /api/account/reset`     `{ token, password }` → `{ username, token }` (neues Passwort, gleich angemeldet)
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
    mailer: crate::mail::Mailer,
}

/// So lange gilt ein Link zum Zurücksetzen.
const RESET_MINUTES: i64 = 60;
/// Frühestens nach so vielen Sekunden geht ein neuer Link an denselben Account.
const RESET_COOLDOWN_SECONDS: i64 = 120;

impl Accounts {
    /// Mit der Datenbank aus `DATABASE_URL` verbinden und Tabellen anlegen.
    /// Klappt das nicht, läuft der Server ohne Accounts weiter.
    pub async fn from_env() -> Accounts {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            info!("Keine DATABASE_URL: Accounts sind aus, gespielt wird mit Browser-Id");
            return Accounts { db: None, mailer: crate::mail::Mailer::from_env() };
        };
        let pool = match PgPoolOptions::new().max_connections(5).acquire_timeout(std::time::Duration::from_secs(5)).connect(&url).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Datenbank nicht erreichbar ({e}): Accounts sind aus");
                return Accounts { db: None, mailer: crate::mail::Mailer::from_env() };
            }
        };
        if let Err(e) = sqlx::migrate!("./migrations").run(&pool).await {
            warn!("Datenbank-Tabellen nicht angelegt ({e}): Accounts sind aus");
            return Accounts { db: None, mailer: crate::mail::Mailer::from_env() };
        }
        info!("Datenbank verbunden, Accounts sind an");
        Accounts { db: Some(pool), mailer: crate::mail::Mailer::from_env() }
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
        .route("/api/account/profile", post(profile))
        .route("/api/account/forgot", post(forgot))
        .route("/api/account/reset", post(reset))
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
    Json(serde_json::json!({ "enabled": acc.db.is_some(), "mail": acc.mailer.enabled() })).into_response()
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Zufälliger Link-Schlüssel (32 Bytes, hex).
fn new_reset_token() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
struct ForgotBody {
    login: String,
    #[serde(default)]
    lang: String,
}

async fn forgot(State(acc): State<Arc<Accounts>>, Json(b): Json<ForgotBody>) -> Response {
    let Some(db) = &acc.db else { return fail(StatusCode::SERVICE_UNAVAILABLE, "account.disabled") };
    let login = b.login.trim();
    let row: Option<(i64, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT a.id, a.username, a.email, EXISTS (SELECT 1 FROM password_resets r WHERE r.account_id = a.id \
           AND r.created_at > now() - make_interval(secs => $2)) \
         FROM accounts a WHERE lower(a.username) = lower($1) OR lower(a.email) = lower($1)",
    )
    .bind(login)
    .bind(RESET_COOLDOWN_SECONDS as f64)
    .fetch_optional(db)
    .await
    .unwrap_or_else(|e| {
        warn!("Passwort vergessen: {e}");
        None
    });
    // Immer dieselbe Antwort: niemand soll so herausfinden, welche Namen oder Adressen es gibt.
    let done = StatusCode::NO_CONTENT.into_response();
    let Some((id, username, Some(email), recent)) = row else { return done };
    if recent {
        return done;
    }
    let token = new_reset_token();
    let _ = sqlx::query("DELETE FROM password_resets WHERE account_id = $1 OR expires_at < now()").bind(id).execute(db).await;
    let stored = sqlx::query("INSERT INTO password_resets (token_hash, account_id, expires_at) VALUES ($1, $2, now() + make_interval(mins => $3))")
        .bind(sha256_hex(&token))
        .bind(id)
        .bind(RESET_MINUTES as i32)
        .execute(db)
        .await;
    if let Err(e) = stored {
        warn!("Passwort vergessen: {e}");
        return done;
    }
    let link = format!("{}/reset.html?token={token}", acc.mailer.public_url);
    let (subject, body) = if b.lang == "en" {
        (
            "Untextured Kingdoms: reset your password".to_string(),
            format!("Hello {username},\n\nsomeone (hopefully you) asked to reset your password. Open this link within {RESET_MINUTES} minutes to choose a new one:\n\n{link}\n\nIf it wasn't you, just ignore this email. Your password stays the same.\n"),
        )
    } else {
        (
            "Untextured Kingdoms: Passwort zurücksetzen".to_string(),
            format!("Hallo {username},\n\njemand (hoffentlich du) möchte dein Passwort zurücksetzen. Öffne innerhalb von {RESET_MINUTES} Minuten diesen Link und wähle ein neues:\n\n{link}\n\nWarst du das nicht, ignoriere diese Mail einfach. Dein Passwort bleibt dann, wie es ist.\n"),
        )
    };
    let mailer = acc.mailer.clone();
    // Im Hintergrund schicken: die Antwortzeit soll nicht verraten, ob es den Account gibt.
    tokio::spawn(async move {
        match mailer.send(&email, &subject, &body).await {
            Ok(()) => info!("Link zum Zurücksetzen an {username} geschickt"),
            Err(e) => warn!("Mail an {username} nicht verschickt: {e}"),
        }
    });
    done
}

#[derive(Deserialize)]
struct ResetBody {
    token: String,
    password: String,
}

async fn reset(State(acc): State<Arc<Accounts>>, Json(b): Json<ResetBody>) -> Response {
    let Some(db) = &acc.db else { return fail(StatusCode::SERVICE_UNAVAILABLE, "account.disabled") };
    if !password_ok(&b.password) {
        return fail(StatusCode::BAD_REQUEST, "account.password_short");
    }
    let Some(hash) = hash_password(b.password).await else { return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error") };
    // Link einlösen (gilt nur einmal) und Passwort setzen.
    let row: Option<(String, String)> = match sqlx::query_as(
        "WITH used AS (DELETE FROM password_resets WHERE token_hash = $1 AND expires_at > now() RETURNING account_id) \
         UPDATE accounts SET password_hash = $2, last_login = now() WHERE id IN (SELECT account_id FROM used) \
         RETURNING username, player_token",
    )
    .bind(sha256_hex(b.token.trim()))
    .bind(&hash)
    .fetch_optional(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("Passwort zurücksetzen: {e}");
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error");
        }
    };
    let Some((username, token)) = row else { return fail(StatusCode::BAD_REQUEST, "account.reset_invalid") };
    info!("Passwort von {username} zurückgesetzt");
    Json(serde_json::json!({ "username": username, "token": token })).into_response()
}

#[derive(Deserialize)]
struct RegisterBody {
    username: String,
    password: String,
    token: String,
    #[serde(default)]
    email: Option<String>,
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
    let email = match clean_email(b.email.as_deref().unwrap_or("")) {
        Ok(e) => e,
        Err(r) => return r,
    };
    let Some(hash) = hash_password(b.password).await else { return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error") };
    let result = sqlx::query("INSERT INTO accounts (username, password_hash, player_token, last_login, email) VALUES ($1, $2, $3, now(), $4)")
        .bind(&username)
        .bind(&hash)
        .bind(&b.token)
        .bind(&email)
        .execute(db)
        .await;
    match result {
        Ok(_) => {
            info!("Account {username} angelegt");
            Json(serde_json::json!({ "username": username, "token": b.token })).into_response()
        }
        Err(e) => unique_error(e, "Account anlegen"),
    }
}

/// Doppelter Name, Token oder E-Mail: passende Meldung, sonst allgemeiner Fehler.
fn unique_error(e: sqlx::Error, what: &str) -> Response {
    match e {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            let key = match e.constraint() {
                Some("accounts_player_token_key") => "account.token_taken",
                Some("accounts_email_lower") => "account.email_taken",
                _ => "account.name_taken",
            };
            fail(StatusCode::CONFLICT, key)
        }
        e => {
            warn!("{what}: {e}");
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
    me_json(db, &b.token).await
}

async fn me_json(db: &PgPool, token: &str) -> Response {
    let row: Option<(String, bool, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT username, is_admin, email, avatar FROM accounts WHERE player_token = $1")
            .bind(token)
            .fetch_optional(db)
            .await
            .unwrap_or(None);
    match row {
        Some((username, admin, email, avatar)) => {
            Json(serde_json::json!({ "username": username, "admin": admin, "email": email, "avatar": avatar })).into_response()
        }
        None => Json(serde_json::json!({ "username": null, "admin": false })).into_response(),
    }
}

/// Größtes Profilbild (als data:-URL). Der Client verkleinert vorher auf 128 × 128.
const MAX_AVATAR: usize = 200 * 1024;

async fn hash_password(password: String) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string())
    })
    .await
    .ok()?
    .ok()
}

fn password_ok(password: &str) -> bool {
    password.chars().count() >= 8 && password.len() <= 200
}

/// Leer = keine E-Mail.
#[allow(clippy::result_large_err)]
fn clean_email(email: &str) -> Result<Option<String>, Response> {
    let email = email.trim();
    if email.is_empty() {
        return Ok(None);
    }
    let valid = email.len() <= 200
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(a, b)| !a.is_empty() && b.contains('.') && !b.starts_with('.') && !b.ends_with('.'));
    if valid {
        Ok(Some(email.to_string()))
    } else {
        Err(fail(StatusCode::BAD_REQUEST, "account.invalid_email"))
    }
}

/// Leer = kein Bild. Nur kleine PNG/JPEG/WebP-Bilder als data:-URL.
#[allow(clippy::result_large_err)]
fn clean_avatar(avatar: &str) -> Result<Option<String>, Response> {
    if avatar.is_empty() {
        return Ok(None);
    }
    let ok_type = ["data:image/png;base64,", "data:image/jpeg;base64,", "data:image/webp;base64,"].iter().any(|p| avatar.starts_with(p));
    let ok_chars = avatar.split_once(',').is_some_and(|(_, d)| d.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
    if !ok_type || !ok_chars {
        return Err(fail(StatusCode::BAD_REQUEST, "account.invalid_avatar"));
    }
    if avatar.len() > MAX_AVATAR {
        return Err(fail(StatusCode::PAYLOAD_TOO_LARGE, "account.avatar_too_big"));
    }
    Ok(Some(avatar.to_string()))
}

#[derive(Deserialize)]
struct ProfileBody {
    token: String,
    username: Option<String>,
    email: Option<String>,
    avatar: Option<String>,
    current_password: Option<String>,
    new_password: Option<String>,
}

async fn profile(State(acc): State<Arc<Accounts>>, Json(b): Json<ProfileBody>) -> Response {
    let Some(db) = &acc.db else { return fail(StatusCode::SERVICE_UNAVAILABLE, "account.disabled") };
    let row: Option<(i64, String, String)> = match sqlx::query_as("SELECT id, username, password_hash FROM accounts WHERE player_token = $1")
        .bind(&b.token)
        .fetch_optional(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("Profil: {e}");
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error");
        }
    };
    let Some((id, old_name, hash)) = row else { return fail(StatusCode::NOT_FOUND, "account.not_logged_in") };
    // Erst alles prüfen, dann in einem Rutsch schreiben.
    let username = match b.username.map(|u| u.trim().to_string()) {
        Some(u) if u != old_name => {
            if !valid_username(&u) {
                return fail(StatusCode::BAD_REQUEST, "account.invalid_name");
            }
            Some(u)
        }
        _ => None,
    };
    let email = match b.email.as_deref().map(clean_email) {
        Some(Ok(e)) => Some(e),
        Some(Err(r)) => return r,
        None => None,
    };
    let avatar = match b.avatar.as_deref().map(clean_avatar) {
        Some(Ok(a)) => Some(a),
        Some(Err(r)) => return r,
        None => None,
    };
    let new_hash = match b.new_password {
        Some(new) if !new.is_empty() => {
            if !password_ok(&new) {
                return fail(StatusCode::BAD_REQUEST, "account.password_short");
            }
            let current = b.current_password.unwrap_or_default();
            let ok = tokio::task::spawn_blocking(move || {
                PasswordHash::new(&hash).is_ok_and(|p| Argon2::default().verify_password(current.as_bytes(), &p).is_ok())
            })
            .await
            .unwrap_or(false);
            if !ok {
                return fail(StatusCode::UNAUTHORIZED, "account.wrong_password");
            }
            let Some(h) = hash_password(new).await else { return fail(StatusCode::INTERNAL_SERVER_ERROR, "account.error") };
            Some(h)
        }
        _ => None,
    };
    let result = sqlx::query(
        "UPDATE accounts SET \
           username = COALESCE($2, username), \
           email = CASE WHEN $3 THEN $4 ELSE email END, \
           avatar = CASE WHEN $5 THEN $6 ELSE avatar END, \
           password_hash = COALESCE($7, password_hash) \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&username)
    .bind(email.is_some())
    .bind(email.flatten())
    .bind(avatar.is_some())
    .bind(avatar.flatten())
    .bind(&new_hash)
    .execute(db)
    .await;
    if let Err(e) = result {
        return unique_error(e, "Profil speichern");
    }
    if let Some(u) = &username {
        info!("Account {old_name} heißt jetzt {u}");
    }
    me_json(db, &b.token).await
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

    #[test]
    fn email_und_bild_pruefen() {
        assert_eq!(clean_email(" jan@example.org ").ok(), Some(Some("jan@example.org".into())));
        assert_eq!(clean_email("").ok(), Some(None));
        assert!(clean_email("kein-at").is_err());
        assert!(clean_email("a@b").is_err());
        assert!(clean_avatar("data:image/png;base64,iVBORw0KGgo=").is_ok());
        assert!(clean_avatar("data:text/html;base64,PGI+").is_err());
        assert!(clean_avatar("data:image/png;base64,<script>").is_err());
        assert!(clean_avatar(&format!("data:image/png;base64,{}", "A".repeat(MAX_AVATAR))).is_err());
    }
}
