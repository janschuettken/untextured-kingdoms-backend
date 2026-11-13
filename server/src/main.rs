//! Autoritativer Spielserver: feste Tick-Rate, Clients per WebSocket.

mod map;

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, MapInfo, ServerMsg};
use tokio::sync::broadcast;
use tracing::{info, warn};

const TICK_RATE: u32 = 20;

struct AppState {
    map: MapInfo,
    next_player_id: AtomicU32,
    players_online: AtomicU32,
    /// Jeder Tick wird an alle verbundenen Clients verteilt.
    ticks: broadcast::Sender<ServerMsg>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let (ticks, _) = broadcast::channel(64);
    let state = Arc::new(AppState {
        map: map::generate(64, 64, 3, 42),
        next_player_id: AtomicU32::new(1),
        players_online: AtomicU32::new(0),
        ticks,
    });

    tokio::spawn(game_loop(state.clone()));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Server läuft auf {addr} ({TICK_RATE} Ticks/s)");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Feste Simulationsschleife. Hier kommt später die eigentliche Spiellogik hin.
async fn game_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1) / TICK_RATE);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tick: u64 = 0;
    loop {
        interval.tick().await;
        tick += 1;
        // Fehler heißt nur: gerade niemand verbunden.
        let _ = state.ticks.send(ServerMsg::Tick {
            tick,
            players_online: state.players_online.load(Ordering::Relaxed),
        });
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let player_id = state.next_player_id.fetch_add(1, Ordering::Relaxed);
    state.players_online.fetch_add(1, Ordering::Relaxed);
    info!("Spieler {player_id} verbunden");

    let (mut tx, mut rx) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<ServerMsg>(64);

    let welcome = ServerMsg::Welcome {
        player_id,
        tick_rate: TICK_RATE,
        map: state.map.clone(),
    };
    let _ = out_tx.send(welcome).await;

    // Ausgehend: direkte Antworten + Ticks an diesen Client schreiben.
    let mut ticks = state.ticks.subscribe();
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                Some(m) = out_rx.recv() => m,
                Ok(m) = ticks.recv() => m,
                else => break,
            };
            let text = serde_json::to_string(&msg).expect("serialisierbar");
            if tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Eingehend: Nachrichten des Clients verarbeiten.
    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(text) = msg else { continue };
        match serde_json::from_str::<ClientMsg>(&text) {
            Ok(ClientMsg::Hello { name }) => info!("Spieler {player_id} heißt {name}"),
            Ok(ClientMsg::Ping { client_time }) => {
                let _ = out_tx.send(ServerMsg::Pong { client_time }).await;
            }
            Err(e) => warn!("Ungültige Nachricht von {player_id}: {e}"),
        }
    }

    writer.abort();
    state.players_online.fetch_sub(1, Ordering::Relaxed);
    info!("Spieler {player_id} getrennt");
}
