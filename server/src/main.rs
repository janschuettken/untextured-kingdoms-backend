//! Autoritativer Spielserver: feste Tick-Rate, Clients per WebSocket.

mod economy;
mod map;
mod mapgen;
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
use protocol::{ClientMsg, MapRef, ServerMsg, UnitState};
use world::World;
use tokio::sync::broadcast;
use tracing::{info, warn};

const TICK_RATE: u32 = 20;
/// So viele Felder um den Sichtbereich herum bekommt ein Client Einheiten
/// mitgeschickt, damit sie beim Verschieben der Kamera schon da sind.
const VIEW_MARGIN: f32 = 24.0;

/// Ein verbundener Client.
struct Client {
    /// Direkter Kanal (Ticks und Nachrichten nur an diesen Spieler).
    tx: tokio::sync::mpsc::Sender<ServerMsg>,
    /// Sichtbereich in Feldern (x0, y0, x1, y1); `None` = alles.
    view: Option<(f32, f32, f32, f32)>,
    /// Was der Client von jeder Einheit zuletzt bekommen hat.
    known: HashMap<u32, UnitState>,
}

struct AppState {
    /// Spielzustand. Nur kurz sperren, nie über ein `.await` hinweg.
    /// Reihenfolge beim Sperren: erst `world`, dann `clients`.
    world: Mutex<World>,
    map_name: String,
    next_player_id: AtomicU32,
    players_online: AtomicU32,
    clients: Mutex<HashMap<u32, Client>>,
    /// Ereignisse für alle (Gebäude, Ressourcen). Ticks gehen pro Client.
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
        world: Mutex::new(World::new(
            game_map,
            economy::Economy::parse(economy::ECONOMY_JSON).expect("economy.json ist gültig"),
        )),
        clients: Mutex::new(HashMap::new()),
        map_name: map_name.clone(),
        next_player_id: AtomicU32::new(1),
        players_online: AtomicU32::new(0),
        ticks,
    });

    tokio::spawn(game_loop(state.clone()));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .route("/api/maps/current", get(current_map))
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

/// Die laufende Karte im Dateiformat mit aktuellem Stand (z. B. abgeholzter Wald).
async fn current_map(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let text = state.world.lock().unwrap().map.to_text();
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], text)
}

/// Hat sich die Einheit so verändert, dass der Client es wissen muss?
fn changed(old: &UnitState, new: &UnitState) -> bool {
    old.moving != new.moving || (old.x - new.x).abs() > 0.001 || (old.y - new.y).abs() > 0.001
}

/// Tick für einen Client: nur Einheiten in seinem Sichtbereich (und die
/// eigenen), und davon nur die neuen oder veränderten.
fn tick_for(player: u32, client: &mut Client, units: &[UnitState]) -> (Vec<UnitState>, Vec<u32>) {
    let visible = |u: &UnitState| {
        u.owner == player
            || client.view.is_none_or(|(x0, y0, x1, y1)| {
                u.x >= x0 - VIEW_MARGIN && u.x <= x1 + VIEW_MARGIN && u.y >= y0 - VIEW_MARGIN && u.y <= y1 + VIEW_MARGIN
            })
    };
    let mut send = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for u in units.iter().filter(|u| visible(u)) {
        seen.insert(u.id);
        if client.known.get(&u.id).is_none_or(|old| changed(old, u)) {
            send.push(u.clone());
        }
    }
    let gone: Vec<u32> = client.known.keys().filter(|id| !seen.contains(id)).copied().collect();
    (send, gone)
}

/// Feste Simulationsschleife.
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
            // Unter dem Lock verschicken, damit neue Clients nichts doppelt oder gar nicht sehen.
            for event in world.take_events() {
                let _ = state.ticks.send(event);
            }
            (world.snapshot(), world.take_dirty_stocks())
        };
        let players_online = state.players_online.load(Ordering::Relaxed);
        let mut clients = state.clients.lock().unwrap();
        for (player, stock) in stocks {
            if let Some(c) = clients.get(&player) {
                let _ = c.tx.try_send(ServerMsg::Stock { stock });
            }
        }
        for (&player, client) in clients.iter_mut() {
            let (send, gone) = tick_for(player, client, &units);
            // Kommt der Client nicht hinterher, fällt der Tick aus; was er
            // nicht bekommen hat, geht dann mit dem nächsten Tick raus.
            let msg = ServerMsg::Tick { tick, players_online, units: send.clone(), gone: gone.clone() };
            if client.tx.try_send(msg).is_ok() {
                for id in gone {
                    client.known.remove(&id);
                }
                for u in send {
                    client.known.insert(u.id, u);
                }
            }
        }
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
    let (map, buildings, economy, stock, mut ticks) = {
        let mut world = state.world.lock().unwrap();
        world.spawn_player(player_id);
        let client = Client { tx: out_tx.clone(), view: None, known: HashMap::new() };
        state.clients.lock().unwrap().insert(player_id, client);
        (
            // Die Karte selbst holt der Client über /api/maps/current.
            MapRef { name: state.map_name.clone(), width: world.map.width, height: world.map.height },
            world.buildings.clone(),
            world.economy.def.clone(),
            world.stock_state(player_id),
            state.ticks.subscribe(),
        )
    };
    info!("Spieler {player_id} verbunden");

    let (mut tx, mut rx) = socket.split();

    let welcome = ServerMsg::Welcome {
        player_id,
        tick_rate: TICK_RATE,
        map,
        economy,
        stock,
        buildings,
    };
    let _ = out_tx.send(welcome).await;

    // Ausgehend: direkte Antworten + Ticks an diesen Client schreiben.
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                // Direkte Nachrichten zuerst, damit `welcome` vor allem anderen ankommt.
                biased;
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
            Ok(ClientMsg::View { x0, y0, x1, y1 }) => {
                if let Some(c) = state.clients.lock().unwrap().get_mut(&player_id) {
                    c.view = Some((x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
                }
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
            Ok(ClientMsg::ConfigureBuilding { building_id, settings }) => {
                let mut world = state.world.lock().unwrap();
                match world.configure(player_id, building_id, settings) {
                    Ok(building) => drop(state.ticks.send(ServerMsg::BuildingChanged { building })),
                    Err(text) => drop(out_tx.try_send(ServerMsg::Rejected { text })),
                }
            }
            Ok(ClientMsg::ReleaseTrees { building_id }) => {
                let mut world = state.world.lock().unwrap();
                match world.release_trees(player_id, building_id) {
                    Ok(()) => {
                        let _ = state.ticks.send(ServerMsg::BuildingRemoved { building_id });
                        for event in world.take_events() {
                            let _ = state.ticks.send(event);
                        }
                    }
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
