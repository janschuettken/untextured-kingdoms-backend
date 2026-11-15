//! Autoritativer Spielserver: Lobby (`/lobby`) und beliebig viele Spiele
//! gleichzeitig, jedes mit eigener Welt und eigenem Thread (`/ws?game=..`).

mod accounts;
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

#[tokio::main]
async fn main() {
    // Lokale Einstellungen (DATABASE_URL, ADMIN_TOKEN …) aus `.env`, falls vorhanden.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = Arc::new(maps_api::MapStore::from_env());
    let admin = admin::Admin::from_env();
    let economy = economy::Economy::load(&admin.economy_file).unwrap_or_else(|e| {
        warn!("Wirtschaftsdaten ungültig ({e}), nehme die eingebauten");
        economy::Economy::parse(economy::ECONOMY_JSON).expect("eingebaute economy.json ist gültig")
    });
    if admin.open() {
        warn!("Kein ADMIN_TOKEN gesetzt: jeder darf Admin-Menü und Balancing-Editor benutzen");
    }
    // Laufende Spiele liegen hier und werden beim Start fortgesetzt.
    let saves_dir = std::env::var("SAVES_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/games"))
        });
    let hub = Arc::new(lobby::Hub::new(store.clone(), admin, economy, saves_dir));
    hub.restore();
    let accounts = accounts::Accounts::from_env().await;

    let app = Router::new()
        .route("/lobby", get(lobby::ws_handler))
        .route("/ws", get(game::ws_handler))
        .route("/health", get(|| async { "ok" }))
        .route("/api/games/{id}/map", get(game::current_map))
        .with_state(hub.clone())
        .merge(maps_api::router(store))
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
        if let Ok(mut s) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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
