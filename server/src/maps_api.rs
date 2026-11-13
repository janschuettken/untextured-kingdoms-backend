//! HTTP-Schnittstelle für den Karteneditor.
//!
//! - `GET  /api/maps`         Namen aller Karten im Kartenordner
//! - `GET  /api/maps/{name}`  Kartendatei als Text
//! - `POST /api/maps/check`   Text prüfen, liefert [`protocol::MapCheck`]
//! - `PUT  /api/maps/{name}`  Karte speichern (nur wenn erlaubt, siehe [`MapStore::can_save`])

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use crate::map::GameMap;

/// Ort der Kartendateien.
pub struct MapStore {
    pub dir: PathBuf,
    /// Speichern über die API erlaubt? Standard: in Debug-Builds (lokal mit
    /// `cargo run`) ja, im Release-Build (Docker) nein. `MAP_EDITOR_SAVE=1/0` überschreibt.
    pub can_save: bool,
}

impl MapStore {
    pub fn from_env() -> MapStore {
        let dir = std::env::var("MAPS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/maps")));
        let can_save = match std::env::var("MAP_EDITOR_SAVE").as_deref() {
            Ok("1") | Ok("true") => true,
            Ok(_) => false,
            Err(_) => cfg!(debug_assertions),
        };
        MapStore { dir, can_save }
    }

    pub fn path(&self, name: &str) -> Option<PathBuf> {
        valid_name(name).then(|| self.dir.join(format!("{name}.txt")))
    }

    pub fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name)?).ok()
    }
}

/// Kartennamen: nur Kleinbuchstaben, Ziffern, `-` und `_`, damit niemand
/// über den Namen aus dem Kartenordner herauskommt.
fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn router<S: Clone + Send + Sync + 'static>(store: Arc<MapStore>) -> Router<S> {
    Router::new()
        .route("/api/maps", get(list))
        .route("/api/maps/check", post(check))
        .route("/api/maps/{name}", get(load).put(save))
        .with_state(store)
}

async fn list(State(store): State<Arc<MapStore>>) -> Response {
    let mut names: Vec<String> = std::fs::read_dir(&store.dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "txt").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .filter(|n| valid_name(n))
        .collect();
    names.sort();
    Json(serde_json::json!({ "maps": names, "can_save": store.can_save })).into_response()
}

async fn load(State(store): State<Arc<MapStore>>, Path(name): Path<String>) -> Response {
    match store.read(&name) {
        Some(text) => text.into_response(),
        None => (StatusCode::NOT_FOUND, "Karte nicht gefunden").into_response(),
    }
}

async fn check(body: String) -> Response {
    Json(GameMap::load(&body).into_check()).into_response()
}

async fn save(State(store): State<Arc<MapStore>>, Path(name): Path<String>, body: String) -> Response {
    if !store.can_save {
        return (StatusCode::FORBIDDEN, "Speichern ist auf diesem Server abgeschaltet (MAP_EDITOR_SAVE)")
            .into_response();
    }
    let Some(path) = store.path(&name) else {
        return (StatusCode::BAD_REQUEST, "Ungültiger Name: nur a-z, 0-9, - und _").into_response();
    };
    let result = GameMap::load(&body).into_check();
    if !result.errors.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(result)).into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&store.dir).and_then(|_| std::fs::write(&path, body)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Speichern fehlgeschlagen: {e}")).into_response();
    }
    tracing::info!("Karte {name} gespeichert");
    Json(result).into_response()
}
