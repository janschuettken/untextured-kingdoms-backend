//! HTTP-Schnittstelle für den Karteneditor.
//!
//! - `GET  /api/maps`         Namen aller Karten im Kartenordner, `can_save`/`can_delete`: darf dieser Spieler speichern/löschen?
//! - `GET  /api/maps/{name}`  Kartendatei als Text
//! - `POST /api/maps/check`   Text prüfen, liefert [`protocol::MapCheck`]
//! - `PUT  /api/maps/{name}`  Karte speichern: nur Admins und Nutzer mit `can_edit_maps` (Kopfzeile `X-Player-Token`, siehe `admin.rs`)
//!   und nur wenn nicht abgeschaltet ([`MapStore::can_save`])
//! - `DELETE /api/maps/{name}` Karte löschen: nur Admins, nicht die letzte. Laufende Spiele haben ihre
//!   Karte im Spielstand und laufen weiter.
//! - `GET  /api/maps/generate?width=..&height=..&players=..&seed=..`  Zufallskarte erzeugen
//!   (optional `farmland forest stone iron hills` in Prozent, `rivers` Anzahl)
//!
//! `GET /api/games/{id}/map` (die laufende Karte eines Spiels mit aktuellem Stand) liegt in `game.rs`.

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use crate::{admin::header_token, lobby::Hub, map::GameMap};

/// Ort der Kartendateien.
pub struct MapStore {
    pub dir: PathBuf,
    /// Speichern über die API überhaupt erlaubt (dann nur für Admins)? Standard ja,
    /// `MAP_EDITOR_SAVE=0` schaltet es ab.
    pub can_save: bool,
}

impl MapStore {
    pub fn from_env() -> MapStore {
        let dir = std::env::var("MAPS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/maps")));
        let can_save = match std::env::var("MAP_EDITOR_SAVE").as_deref() {
            Ok("0") | Ok("false") | Ok("off") => false,
            _ => true,
        };
        let store = MapStore { dir, can_save };
        // Docker: Karten aus dem Image ins Volume, wenn sie dort fehlen und nicht gelöscht wurden.
        if let Ok(seed) = std::env::var("MAPS_SEED_DIR") {
            store.seed_from(std::path::Path::new(&seed));
        }
        store
    }

    /// Hier merkt sich der Ordner gelöschte Karten (je Name eine leere Datei),
    /// damit sie beim nächsten Start nicht aus `MAPS_SEED_DIR` wiederkommen.
    fn deleted_dir(&self) -> PathBuf {
        self.dir.join(".deleted")
    }

    fn seed_from(&self, seed: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(seed) else {
            return;
        };
        let _ = std::fs::create_dir_all(&self.dir);
        for e in entries.flatten() {
            let src = e.path();
            let Some(name) = src
                .file_stem()
                .and_then(|n| n.to_str())
                .filter(|_| src.extension().is_some_and(|x| x == "txt"))
            else {
                continue;
            };
            let Some(dst) = self.path(name) else { continue };
            if dst.exists() || self.deleted_dir().join(name).exists() {
                continue;
            }
            match std::fs::copy(&src, &dst) {
                Ok(_) => tracing::info!("Karte {name} aus {} übernommen", seed.display()),
                Err(e) => tracing::warn!("Karte {name} übernehmen: {e}"),
            }
        }
    }

    /// Karte löschen und als gelöscht merken.
    pub fn delete(&self, name: &str) -> Result<(), (StatusCode, String)> {
        let path = self
            .path(name)
            .filter(|p| p.exists())
            .ok_or((StatusCode::NOT_FOUND, "Karte nicht gefunden".to_string()))?;
        if self.names().len() <= 1 {
            return Err((StatusCode::CONFLICT, "Die letzte Karte bleibt".to_string()));
        }
        let err = |e: std::io::Error| (StatusCode::INTERNAL_SERVER_ERROR, format!("Löschen fehlgeschlagen: {e}"));
        std::fs::create_dir_all(self.deleted_dir()).map_err(err)?;
        std::fs::write(self.deleted_dir().join(name), "").map_err(err)?;
        std::fs::remove_file(path).map_err(err)?;
        Ok(())
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

pub fn router<S: Clone + Send + Sync + 'static>(hub: Arc<Hub>) -> Router<S> {
    Router::new()
        .route("/api/maps", get(list))
        .route("/api/maps/check", post(check))
        .route("/api/maps/generate", get(generate))
        .route("/api/maps/{name}", get(load).put(save).delete(remove))
        // 1024 × 1024 Felder mit Ressourcen sind gut 2 MB Text.
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(hub)
}

async fn list(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let r = hub.admin.rights(header_token(&headers)).await;
    let on = hub.store.can_save;
    Json(serde_json::json!({
        "maps": hub.store.names(),
        "can_save": on && (r.admin || r.maps),
        "can_delete": on && r.admin,
    }))
    .into_response()
}

impl MapStore {
    /// Namen aller Karten im Kartenordner, sortiert.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
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
        names
    }
}

async fn load(State(hub): State<Arc<Hub>>, Path(name): Path<String>) -> Response {
    match hub.store.read(&name) {
        Some(text) => text.into_response(),
        None => (StatusCode::NOT_FOUND, "Karte nicht gefunden").into_response(),
    }
}

/// Prüfen dauert bei großen Karten einige Millisekunden, deshalb nicht auf
/// dem Netzwerk-Thread.
async fn check_text(body: String) -> protocol::MapCheck {
    tokio::task::spawn_blocking(move || GameMap::load(&body).into_check()).await.expect("Prüfung")
}

async fn check(body: String) -> Response {
    Json(check_text(body).await).into_response()
}

#[derive(serde::Deserialize)]
struct GenerateParams {
    width: Option<usize>,
    height: Option<usize>,
    players: Option<usize>,
    seed: Option<u64>,
    /// Regler in Prozent (0–200, 100 = normal).
    farmland: Option<f32>,
    forest: Option<f32>,
    stone: Option<f32>,
    iron: Option<f32>,
    hills: Option<f32>,
    rivers: Option<usize>,
}

async fn generate(Query(p): Query<GenerateParams>) -> Response {
    let pct = |v: Option<f32>| v.map_or(1.0, |v| v / 100.0);
    let mut params = crate::mapgen::Params::new(
        p.width.unwrap_or(256).clamp(32, crate::map::MAX_SIZE),
        p.height.unwrap_or(256).clamp(32, crate::map::MAX_SIZE),
        p.players.unwrap_or(2).clamp(1, 8),
        p.seed.unwrap_or(1),
    );
    params.farmland = pct(p.farmland);
    params.forest = pct(p.forest);
    params.stone = pct(p.stone);
    params.iron = pct(p.iron);
    params.hills = pct(p.hills);
    if let Some(r) = p.rivers {
        params.rivers = r.min(crate::mapgen::MAX_RIVERS);
    }
    tokio::task::spawn_blocking(move || crate::mapgen::generate(&params)).await.expect("Generator").into_response()
}

async fn save(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: String,
) -> Response {
    let store = &hub.store;
    if !store.can_save {
        return (StatusCode::FORBIDDEN, "Speichern ist auf diesem Server abgeschaltet (MAP_EDITOR_SAVE)")
            .into_response();
    }
    if !hub.admin.can_edit_maps(header_token(&headers)).await {
        return (StatusCode::UNAUTHORIZED, "Karten speichern nur mit Recht „Karteneditor“").into_response();
    }
    let Some(path) = store.path(&name) else {
        return (StatusCode::BAD_REQUEST, "Ungültiger Name: nur a-z, 0-9, - und _").into_response();
    };
    let result = check_text(body.clone()).await;
    if !result.errors.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(result)).into_response();
    }
    // Wer eine gelöschte Karte neu speichert, will sie wieder haben.
    let _ = std::fs::remove_file(store.deleted_dir().join(&name));
    if let Err(e) = std::fs::create_dir_all(&store.dir).and_then(|_| std::fs::write(&path, body)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Speichern fehlgeschlagen: {e}")).into_response();
    }
    tracing::info!("Karte {name} gespeichert");
    Json(result).into_response()
}

async fn remove(State(hub): State<Arc<Hub>>, headers: HeaderMap, Path(name): Path<String>) -> Response {
    if !hub.store.can_save {
        return (StatusCode::FORBIDDEN, "Karten ändern ist auf diesem Server abgeschaltet (MAP_EDITOR_SAVE)")
            .into_response();
    }
    if !hub.admin.check(header_token(&headers)).await {
        return (StatusCode::UNAUTHORIZED, "Karten löschen nur für Admins").into_response();
    }
    match hub.store.delete(&name) {
        Ok(()) => {
            tracing::info!("Karte {name} gelöscht");
            StatusCode::NO_CONTENT.into_response()
        }
        Err((status, msg)) => (status, msg).into_response(),
    }
}
