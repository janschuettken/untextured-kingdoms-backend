//! Admin: Token-Prüfung und Balancing-Schnittstelle.
//!
//! Der Admin-Token kommt aus der Umgebungsvariable `ADMIN_TOKEN`. Ist sie
//! nicht gesetzt, gibt es keinen Admin: Waren geben und Speichern sind dann aus.
//!
//! - `GET  /api/admin`    `{ "enabled": bool }`: gibt es überhaupt einen Admin?
//! - `POST /api/admin`    Token prüfen (Kopfzeile `X-Admin-Token`), 204 oder 401
//! - `GET  /api/economy`  die geltenden Wirtschaftsdaten als JSON
//! - `PUT  /api/economy`  nur Admin: neue Daten prüfen, in die Datei schreiben und
//!   sofort für alle übernehmen (`economy_changed` an alle Clients)
//!
//! Die Datei ist `ECONOMY_FILE`, Standard `server/data/economy.json`. Vor dem
//! Überschreiben bleibt die alte Fassung als `economy.json.bak` liegen.

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
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

    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    /// Stimmt der Token? Vergleich ohne früh abzubrechen, damit die Laufzeit nichts verrät.
    pub fn check(&self, given: &str) -> bool {
        let Some(token) = &self.token else { return false };
        let (a, b) = (token.as_bytes(), given.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (i, &x) in a.iter().enumerate() {
            diff |= (x ^ b.get(i).copied().unwrap_or(0)) as usize;
        }
        diff == 0
    }

    fn allows(&self, headers: &HeaderMap) -> bool {
        headers.get("x-admin-token").and_then(|v| v.to_str().ok()).is_some_and(|t| self.check(t))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/admin", get(status).post(login))
        .route("/api/economy", get(load).put(save))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

async fn status(State(state): State<Arc<AppState>>) -> Response {
    Json(serde_json::json!({ "enabled": state.admin.enabled() })).into_response()
}

async fn login(State(state): State<Arc<AppState>>, headers: HeaderMap) -> StatusCode {
    if state.admin.allows(&headers) { StatusCode::NO_CONTENT } else { StatusCode::UNAUTHORIZED }
}

async fn load(State(state): State<Arc<AppState>>) -> Response {
    let text = state.world.lock().unwrap().economy.to_json();
    ([(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")], text).into_response()
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
        assert!(!none.check(""), "ohne ADMIN_TOKEN gibt es keinen Admin");
    }
}
