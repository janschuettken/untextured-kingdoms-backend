//! Autoritativer Spielserver: feste Tick-Rate, Clients per WebSocket.

mod economy;
mod map;
mod maps_api;
mod nav;
mod stock;
mod world;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
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
use world::World;
use tokio::sync::broadcast;
use tracing::{info, warn};

const TICK_RATE: u32 = 20;

struct AppState {
    /// Kartendaten für neue Clients (ändert sich nicht).
    map: MapInfo,
    /// Spielzustand. Nur kurz sperren, nie über ein `.await` hinweg.
    world: Mutex<World>,
    next_player_id: AtomicU32,
    players_online: AtomicU32,
    /// Direkter Kanal zu jedem Client (für Nachrichten nur an einen Spieler).
    clients: Mutex<HashMap<u32, tokio::sync::mpsc::Sender<ServerMsg>>>,
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
    // Karte aus dem Kartenordner laden: MAP=name (ohne .txt), Standard "zwei-burgen".
    let store = Arc::new(maps_api::MapStore::from_env());
    let map_name = std::env::var("MAP").unwrap_or_else(|_| "zwei-burgen".into());
    let map_text = store.read(&map_name).unwrap_or_else(|| {
        warn!("Karte {map_name} nicht in {} gefunden, nehme eingebaute Standardkarte", store.dir.display());
        map::DEFAULT_MAP.to_string()
    });
    let game_map = map::GameMap::parse(&map_text)
        .unwrap_or_else(|e| panic!("Karte {map_name} ist ungültig: {e}"));
    info!("Karte {map_name} geladen ({} x {})", game_map.width, game_map.height);
    let state = Arc::new(AppState {
        map: game_map.info(),
        world: Mutex::new(World::new(
            game_map,
            economy::Economy::parse(economy::ECONOMY_JSON).expect("economy.json ist gültig"),
        )),
        clients: Mutex::new(HashMap::new()),
        next_player_id: AtomicU32::new(1),
        players_online: AtomicU32::new(0),
        ticks,
    });

    tokio::spawn(game_loop(state.clone()));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
        .merge(maps_api::router(store));

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
        let (units, stocks) = {
            let mut world = state.world.lock().unwrap();
            world.step(1.0 / TICK_RATE as f32);
            (world.snapshot(), world.take_dirty_stocks())
        };
        if !stocks.is_empty() {
            let clients = state.clients.lock().unwrap();
            for (player, stock) in stocks {
                if let Some(tx) = clients.get(&player) {
                    let _ = tx.try_send(ServerMsg::Stock { stock });
                }
            }
        }
        // Fehler heißt nur: gerade niemand verbunden.
        let _ = state.ticks.send(ServerMsg::Tick {
            tick,
            players_online: state.players_online.load(Ordering::Relaxed),
            units,
        });
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let player_id = state.next_player_id.fetch_add(1, Ordering::Relaxed);
    state.players_online.fetch_add(1, Ordering::Relaxed);
    // Gebäudestand und Abo im selben Lock holen: so geht keine Änderung
    // verloren und keine kommt doppelt an.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<ServerMsg>(64);
    let (buildings, economy, stock, mut ticks) = {
        let mut world = state.world.lock().unwrap();
        world.spawn_player(player_id);
        state.clients.lock().unwrap().insert(player_id, out_tx.clone());
        (world.buildings.clone(), world.economy.def.clone(), world.stock_state(player_id), state.ticks.subscribe())
    };
    info!("Spieler {player_id} verbunden");

    let (mut tx, mut rx) = socket.split();

    let welcome = ServerMsg::Welcome {
        player_id,
        tick_rate: TICK_RATE,
        map: state.map.clone(),
        economy,
        stock,
        buildings,
    };
    let _ = out_tx.send(welcome).await;

    // Ausgehend: direkte Antworten + Ticks an diesen Client schreiben.
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
            Ok(ClientMsg::MoveUnits { unit_ids, x, y }) => {
                state.world.lock().unwrap().order_move(player_id, &unit_ids, (x, y));
            }
            Ok(ClientMsg::PlaceBuilding { kind, x, y }) => {
                let mut world = state.world.lock().unwrap();
                match world.place_building(player_id, kind, x, y) {
                    // Noch unter dem Lock verschicken, damit die Reihenfolge stimmt.
                    Ok(building) => drop(state.ticks.send(ServerMsg::BuildingPlaced { building })),
                    Err(text) => drop(out_tx.try_send(ServerMsg::Rejected { text })),
                }
            }
            Ok(ClientMsg::DemolishBuilding { building_id }) => {
                let mut world = state.world.lock().unwrap();
                match world.demolish(player_id, building_id) {
                    Ok(()) => drop(state.ticks.send(ServerMsg::BuildingRemoved { building_id })),
                    Err(text) => drop(out_tx.try_send(ServerMsg::Rejected { text })),
                }
            }
            Ok(ClientMsg::UpgradeBuilding { building_id }) => {
                let mut world = state.world.lock().unwrap();
                match world.upgrade(player_id, building_id) {
                    Ok(building) => drop(state.ticks.send(ServerMsg::BuildingChanged { building })),
                    Err(text) => drop(out_tx.try_send(ServerMsg::Rejected { text })),
                }
            }
            Err(e) => warn!("Ungültige Nachricht von {player_id}: {e}"),
        }
    }

    writer.abort();
    state.clients.lock().unwrap().remove(&player_id);
    {
        let mut world = state.world.lock().unwrap();
        for building_id in world.remove_player(player_id) {
            let _ = state.ticks.send(ServerMsg::BuildingRemoved { building_id });
        }
    }
    state.players_online.fetch_sub(1, Ordering::Relaxed);
    info!("Spieler {player_id} getrennt");
}
