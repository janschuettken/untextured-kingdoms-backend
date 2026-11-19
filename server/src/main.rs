//! Autoritativer Spielserver: Lobby (`/lobby`) und beliebig viele Spiele
//! gleichzeitig, jedes mit eigener Welt und eigenem Thread (`/ws?game=..`).

mod accounts;
mod ai;
mod admin;
mod economy;
mod game;
mod lobby;
mod map;
mod mapgen;
mod maps_api;
mod nav;
mod stock;
mod world;

use std::{net::SocketAddr, sync::Arc};

use axum::{routing::get, Router};
use tracing::{info, warn};

/// Version des Servers: Commit und Datum (siehe `build.rs`), für die Anzeige in der Lobby.
pub const VERSION: &str = env!("UK_VERSION");

#[tokio::main]
async fn main() {
    // `server economy-canonical < datei`: economy.json im Format des Servers ausgeben
    // (für deploy/update.sh, um Server- und Repo-Fassung zu vergleichen).
    if std::env::args().nth(1).as_deref() == Some("economy-canonical") {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).expect("stdin lesen");
        match economy::Economy::parse(&text) {
            Ok(e) => print!("{}", e.to_json()),
            Err(e) => {
                eprintln!("ungültig: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // Lokale Einstellungen (DATABASE_URL, ADMIN_USERS …) aus `.env`, falls vorhanden.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = Arc::new(maps_api::MapStore::from_env());
    // Accounts zuerst: an ihnen hängen die Admin-Rechte.
    let accounts = accounts::Accounts::from_env().await;
    let admin = admin::Admin::from_env(accounts.pool()).await;
    let economy = economy::Economy::load(&admin.economy_file).unwrap_or_else(|e| {
        warn!("Wirtschaftsdaten ungültig ({e}), nehme die eingebauten");
        economy::Economy::parse(economy::ECONOMY_JSON).expect("eingebaute economy.json ist gültig")
    });
    // Laufende Spiele liegen hier und werden beim Start fortgesetzt.
    let saves_dir = std::env::var("SAVES_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/games"))
        });
    let hub = Arc::new(lobby::Hub::new(store, admin, economy, saves_dir));
    match ai::personality::load(&hub.admin.personalities_file) {
        Ok(all) => *hub.personalities.lock().unwrap() = all,
        Err(e) => warn!("Charaktere der Computergegner ungültig ({e}), nehme die eingebauten"),
    }
    hub.restore();

    let app = Router::new()
        .route("/lobby", get(lobby::ws_handler))
        .route("/ws", get(game::ws_handler))
        .route("/health", get(|| async { "ok" }))
        .route("/api/games/{id}/map", get(game::current_map))
        .with_state(hub.clone())
        .merge(maps_api::router(hub.clone()))
        .merge(admin::router(hub.clone()))
        .merge(accounts::router(accounts));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(
        "Server läuft auf {addr} ({} Ticks/s je Spiel)",
        game::TICK_RATE
    );
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    // Bei Strg+C oder SIGTERM (docker stop) erst alle Spiele speichern, dann beenden.
    // Offene WebSockets halten den Server nicht auf: nach dem Signal sofort speichern und beenden.
    tokio::select! {
        r = axum::serve(listener, app) => r.unwrap(),
        _ = shutdown_signal() => {},
    }
    info!("Server wird beendet, speichere laufende Spiele");
    hub.save_all();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
