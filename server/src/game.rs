//! Ein laufendes Spiel: eigene Welt, eigener Thread mit fester Tick-Rate,
//! eigene WebSocket-Clients (`/ws?game=..&token=..`).
//!
//! Wer mitspielt, steht in der Lobby (`lobby.rs`); hier wird nur geprüft,
//! ob der Token zu einem Startplatz dieses Spiels gehört.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, MapRef, ServerMsg, UnitInfo, UnitState};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{
    ai::{self, Computer},
    lobby::{Game, Hub},
    world::World,
};

pub const TICK_RATE: u32 = 20;
/// So viele Felder um den Sichtbereich herum bekommt ein Client Einheiten
/// mitgeschickt, damit sie beim Verschieben der Kamera schon da sind.
const VIEW_MARGIN: f32 = 24.0;
/// Alle so viele Sekunden wird jedes laufende Spiel automatisch gespeichert.
const AUTOSAVE_SECONDS: u64 = 60;

/// Ein verbundener Client.
pub struct Client {
    /// Welche Verbindung (bei zwei Fenstern desselben Spielers gewinnt die neue).
    conn: u64,
    /// Direkter Kanal (Ticks und Nachrichten nur an diesen Spieler).
    tx: tokio::sync::mpsc::Sender<ServerMsg>,
    /// Sichtbereich in Feldern (x0, y0, x1, y1); `None` = alles.
    view: Option<(f32, f32, f32, f32)>,
    /// Was der Client von jeder Einheit zuletzt bekommen hat.
    known: HashMap<u32, UnitState>,
    /// Beobachtete Einheit (Weg anzeigen) und der zuletzt geschickte Weg.
    watch: Option<u32>,
    watch_sent: Option<Vec<(f32, f32)>>,
    /// Zuletzt geschickte `UnitInfo` der beobachteten Einheit.
    watch_info: Option<UnitInfo>,
}

/// Zustand eines laufenden Spiels.
pub struct Match {
    /// Spielzustand. Nur kurz sperren, nie über ein `.await` hinweg.
    /// Reihenfolge beim Sperren: erst `world`, dann `clients`.
    pub world: Mutex<World>,
    pub map_name: String,
    /// Kartentext beim Start (für Neues Spiel und zum Speichern).
    pub map_text: String,
    pub clients: Mutex<HashMap<u32, Client>>,
    /// Ereignisse für alle (Gebäude, Ressourcen). Ticks gehen pro Client.
    pub ticks: broadcast::Sender<ServerMsg>,
    /// Spieltempo in Prozent (100 = normal), vom Admin einstellbar.
    pub game_speed: AtomicU32,
    /// Gesetzt, wenn das Spiel beendet wird: der Thread hört auf.
    pub stop: AtomicBool,
    next_conn: AtomicU64,
    /// Computergegner in diesem Spiel. Reihenfolge beim Sperren: nach `world`.
    pub(crate) computers: Mutex<Vec<Computer>>,
}

impl Match {
    pub fn new(world: World, map_name: String, map_text: String, speed: u32) -> Match {
        // Großzügig: pro Tick können viele Gebäude melden (Lager, Status).
        let (ticks, _) = broadcast::channel(4096);
        Match {
            world: Mutex::new(world),
            map_name,
            map_text,
            clients: Mutex::new(HashMap::new()),
            ticks,
            game_speed: AtomicU32::new(speed),
            stop: AtomicBool::new(false),
            next_conn: AtomicU64::new(1),
            computers: Mutex::new(Vec::new()),
        }
    }

    /// Diese Startplätze spielt der Computer (Spieler-Nummer, Stärke).
    pub fn set_computers(&mut self, list: &[(u32, protocol::AiLevel)]) {
        *self.computers.get_mut().unwrap() = ai::computers(list);
    }

    pub fn is_online(&self, player: u32) -> bool {
        self.clients.lock().unwrap().contains_key(&player)
    }

    pub fn online_players(&self) -> Vec<u32> {
        self.clients.lock().unwrap().keys().copied().collect()
    }

    /// Alle Clients rauswerfen (Spiel beendet): sie gehen zurück in die Lobby.
    pub fn close_all(&self) {
        for c in self.clients.lock().unwrap().values() {
            let _ = c.tx.try_send(ServerMsg::NotInGame);
        }
    }

    /// Neue Welt einsetzen und alle Clients neu laden lassen.
    pub fn replace_world(&self, world: World) {
        *self.world.lock().unwrap() = world;
        let _ = self.ticks.send(ServerMsg::Reload);
    }
}

/// Eigener Thread pro Spiel, damit ein großes Spiel die anderen nicht ausbremst.
pub fn spawn_thread(hub: Arc<Hub>, game: Arc<Game>, m: Arc<Match>) {
    let name = format!("spiel-{}", game.id);
    std::thread::Builder::new()
        .name(name)
        .spawn(move || game_loop(hub, game, m))
        .expect("Spiel-Thread starten");
}

/// Hat sich die Einheit so verändert, dass der Client es wissen muss?
fn changed(old: &UnitState, new: &UnitState) -> bool {
    old.moving != new.moving
        || old.carrying != new.carrying
        || (old.x - new.x).abs() > 0.001
        || (old.y - new.y).abs() > 0.001
}

/// Tick für einen Client: nur Einheiten in seinem Sichtbereich (und die
/// eigenen), und davon nur die neuen oder veränderten.
fn tick_for(player: u32, client: &mut Client, units: &[UnitState]) -> (Vec<UnitState>, Vec<u32>) {
    let visible = |u: &UnitState| {
        u.owner == player
            || client.view.is_none_or(|(x0, y0, x1, y1)| {
                u.x >= x0 - VIEW_MARGIN
                    && u.x <= x1 + VIEW_MARGIN
                    && u.y >= y0 - VIEW_MARGIN
                    && u.y <= y1 + VIEW_MARGIN
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
    let gone: Vec<u32> = client
        .known
        .keys()
        .filter(|id| !seen.contains(id))
        .copied()
        .collect();
    (send, gone)
}

/// Feste Simulationsschleife eines Spiels, bis `stop` gesetzt ist.
fn game_loop(hub: Arc<Hub>, game: Arc<Game>, state: Arc<Match>) {
    let dt = Duration::from_secs(1) / TICK_RATE;
    let mut next = Instant::now();
    let mut tick: u64 = 0;
    info!("Spiel {} läuft (eigener Thread)", game.id);
    while !state.stop.load(Ordering::Relaxed) {
        next += dt;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > dt {
            // Zu weit hinterher (Rechner war beschäftigt): nicht nachholen.
            next = now;
        }
        tick += 1;
        if tick.is_multiple_of(AUTOSAVE_SECONDS * TICK_RATE as u64) {
            if let Err(e) = hub.save_game(&game) {
                warn!(
                    "Spiel {}: automatisches Speichern fehlgeschlagen: {e}",
                    game.id
                );
            }
        }
        step(&state, tick);
    }
    info!("Spiel {} beendet", game.id);
}

fn step(state: &Match, tick: u64) {
    let (units, stocks, paths, infos) = {
        let mut world = state.world.lock().unwrap();
        let speed = state.game_speed.load(Ordering::Relaxed) as f32 / 100.0;
        let dt = speed / TICK_RATE as f32;
        // Computergegner geben ihre Befehle wie Menschen, über denselben Weg.
        for computer in state.computers.lock().unwrap().iter_mut() {
            for cmd in computer.think(&world, dt) {
                if let Err(text) = apply_command(&mut world, &state.ticks, computer.player, cmd) {
                    computer.rejected(&text);
                }
            }
        }
        world.step(dt);
        // Unter dem Lock verschicken, damit neue Clients nichts doppelt oder gar nicht sehen.
        for event in world.take_events() {
            let _ = state.ticks.send(event);
        }
        let watched: Vec<u32> = state
            .clients
            .lock()
            .unwrap()
            .values()
            .filter_map(|c| c.watch)
            .collect();
        let paths: HashMap<u32, Vec<(f32, f32)>> = watched
            .iter()
            .filter_map(|&id| world.unit_path(id).map(|p| (id, p)))
            .collect();
        let infos: HashMap<u32, (u32, UnitInfo)> = watched
            .into_iter()
            .filter_map(|id| Some((id, (world.unit_owner(id)?, world.unit_info(id)?))))
            .collect();
        (world.snapshot(), world.take_dirty_stocks(), paths, infos)
    };
    let mut clients = state.clients.lock().unwrap();
    let players_online = clients.len() as u32;
    for (player, stock) in stocks {
        if let Some(c) = clients.get(&player) {
            let _ = c.tx.try_send(ServerMsg::Stock { stock });
        }
    }
    for (&player, client) in clients.iter_mut() {
        if let Some(id) = client.watch {
            let path = paths.get(&id).cloned().unwrap_or_default();
            if client.watch_sent.as_ref() != Some(&path)
                && client
                    .tx
                    .try_send(ServerMsg::UnitPath {
                        unit_id: id,
                        path: path.clone(),
                    })
                    .is_ok()
            {
                client.watch_sent = Some(path);
            }
            // Tätigkeit und Ladung nur für eigene Einheiten.
            if let Some((owner, info)) = infos.get(&id) {
                if *owner == player
                    && client.watch_info.as_ref() != Some(info)
                    && client
                        .tx
                        .try_send(ServerMsg::UnitInfo { info: info.clone() })
                        .is_ok()
                {
                    client.watch_info = Some(info.clone());
                }
            }
        }
        let (send, gone) = tick_for(player, client, &units);
        // Kommt der Client nicht hinterher, fällt der Tick aus; was er
        // nicht bekommen hat, geht dann mit dem nächsten Tick raus.
        let msg = ServerMsg::Tick {
            tick,
            players_online,
            units: send.clone(),
            gone: gone.clone(),
        };
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

#[derive(serde::Deserialize)]
pub struct GameQuery {
    game: Option<u32>,
    token: Option<String>,
    /// Nur Admin (zum Testen): als dieser Spieler des Spiels spielen.
    #[serde(rename = "as")]
    play_as: Option<u32>,
}

/// Die laufende Karte eines Spiels im Dateiformat mit aktuellem Stand (z. B. abgeholzter Wald).
/// Nur für Mitspieler.
pub async fn current_map(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<u32>,
    Query(q): Query<GameQuery>,
) -> Response {
    let Some((_, m, _)) = q.token.as_deref().and_then(|t| hub.membership(id, t)) else {
        return (StatusCode::NOT_FOUND, "Spiel nicht gefunden").into_response();
    };
    let text = m.world.lock().unwrap().map.to_text();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(hub): State<Arc<Hub>>,
    Query(q): Query<GameQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

async fn handle_socket(mut socket: WebSocket, hub: Arc<Hub>, q: GameQuery) {
    // Nur wer in diesem laufenden Spiel einen Startplatz hat, kommt hinein.
    let found = q
        .game
        .zip(q.token.as_deref())
        .and_then(|(id, token)| hub.membership(id, token));
    // Admin "Spielen als": anderer Startplatz desselben Spiels (Admin ist, wem der eigene Token gehört).
    let may_play_as =
        q.play_as.is_some() && hub.admin.check(q.token.as_deref().unwrap_or("")).await;
    let found = found.and_then(|(game, state, seat)| {
        let Some(pid) = q.play_as else {
            return Some((game, state, seat));
        };
        if !may_play_as {
            return None;
        }
        let spawn = *game
            .meta
            .lock()
            .unwrap()
            .spawns
            .get((pid as usize).checked_sub(1)?)?;
        Some((game, state, (pid, spawn)))
    });
    let Some((game, state, (player_id, spawn))) = found else {
        let text = serde_json::to_string(&ServerMsg::NotInGame).expect("serialisierbar");
        let _ = socket.send(Message::Text(text.into())).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let conn = state.next_conn.fetch_add(1, Ordering::Relaxed);
    // Gebäudestand und Abo im selben Lock holen: so geht keine Änderung
    // verloren und keine kommt doppelt an.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<ServerMsg>(64);
    let (map, buildings, economy, stock, mut ticks) = {
        let mut world = state.world.lock().unwrap();
        // Erstes Mal im Spiel (oder nach "Neues Spiel"): König aufs eigene Startfeld.
        if !world.has_player(player_id) {
            world.spawn_player_at(player_id, spawn);
        }
        let client = Client {
            conn,
            tx: out_tx.clone(),
            view: None,
            known: HashMap::new(),
            watch: None,
            watch_sent: None,
            watch_info: None,
        };
        // Ist derselbe Spieler schon in einem anderen Fenster drin, übernimmt dieses.
        if let Some(old) = state.clients.lock().unwrap().insert(player_id, client) {
            let _ = old.tx.try_send(ServerMsg::OpenedElsewhere);
        }
        (
            // Die Karte selbst holt der Client über /api/games/{id}/map.
            MapRef {
                name: state.map_name.clone(),
                width: world.map.width,
                height: world.map.height,
            },
            world.buildings.clone(),
            world.economy.def.clone(),
            world.stock_state(player_id),
            state.ticks.subscribe(),
        )
    };
    info!("Spiel {}: Spieler {player_id} verbunden", game.id);
    hub.notify();

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
    let _ = out_tx
        .send(ServerMsg::GameSpeed {
            percent: state.game_speed.load(Ordering::Relaxed),
        })
        .await;

    // Ausgehend: direkte Antworten + Ticks an diesen Client schreiben.
    let mut writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                // Direkte Nachrichten zuerst, damit `welcome` vor allem anderen ankommt.
                biased;
                m = out_rx.recv() => match m {
                    Some(m) => m,
                    None => break,
                },
                r = ticks.recv() => match r {
                    Ok(m) => m,
                    // Zu langsam gelesen: Ereignisse (gebaut, abgerissen …) sind verloren
                    // und der Client wäre dauerhaft falsch. Verbindung schließen, der
                    // Client verbindet neu und bekommt mit `welcome` den ganzen Stand.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Spieler {player_id}: {n} Ereignisse verpasst, trenne zum Neuladen");
                        let _ = tx.send(Message::Close(None)).await;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            let last = matches!(msg, ServerMsg::NotInGame | ServerMsg::OpenedElsewhere);
            let text = serde_json::to_string(&msg).expect("serialisierbar");
            if tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
            if last {
                let _ = tx.send(Message::Close(None)).await;
                break;
            }
        }
    });

    // Eingehend: Nachrichten des Clients verarbeiten.
    let reader = async {
        let mut is_admin = false;
        while let Some(Ok(msg)) = rx.next().await {
            let Message::Text(text) = msg else { continue };
            match serde_json::from_str::<ClientMsg>(&text) {
                Ok(msg) => handle_msg(&hub, &state, player_id, &out_tx, &mut is_admin, msg).await,
                Err(e) => warn!("Ungültige Nachricht von {player_id}: {e}"),
            }
        }
    };
    // Endet der Schreiber (Verbindung tot oder absichtlich getrennt), ist auch
    // Schluss mit Lesen: sonst bliebe der Spieler bei halb offenen Verbindungen "online".
    tokio::select! {
        _ = reader => {}
        _ = &mut writer => {}
    }

    writer.abort();
    // Der Spieler bleibt mit allem stehen; er kann später weiterspielen.
    // Nur austragen, wenn nicht schon ein neueres Fenster übernommen hat.
    {
        let mut clients = state.clients.lock().unwrap();
        if clients.get(&player_id).is_some_and(|c| c.conn == conn) {
            clients.remove(&player_id);
        }
    }
    info!("Spiel {}: Spieler {player_id} getrennt", game.id);
    hub.notify();
}

async fn handle_msg(
    hub: &Hub,
    state: &Match,
    player_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<ServerMsg>,
    is_admin: &mut bool,
    msg: ClientMsg,
) {
    match msg {
        ClientMsg::Hello { .. } => {}
        ClientMsg::Ping { client_time } => {
            let _ = out_tx.send(ServerMsg::Pong { client_time }).await;
        }
        ClientMsg::View { x0, y0, x1, y1 } => {
            if let Some(c) = state.clients.lock().unwrap().get_mut(&player_id) {
                c.view = Some((x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
            }
        }
        // Nicht über `apply_command`: nur der Mensch bekommt das rote X bei unerreichbarem Ziel.
        ClientMsg::MoveUnits { unit_ids, x, y } => {
            let reachable = state
                .world
                .lock()
                .unwrap()
                .order_move(player_id, &unit_ids, (x, y));
            if !reachable {
                drop(out_tx.try_send(ServerMsg::MoveUnreachable { x, y }));
            }
        }
        ClientMsg::WatchUnit { unit_id } => {
            if let Some(c) = state.clients.lock().unwrap().get_mut(&player_id) {
                c.watch = unit_id;
                c.watch_sent = None;
                c.watch_info = None;
            }
        }
        msg @ (ClientMsg::PlaceBuilding { .. }
        | ClientMsg::DemolishBuilding { .. }
        | ClientMsg::ConfigureBuilding { .. }
        | ClientMsg::ReleaseTrees { .. }
        | ClientMsg::MoveBuilding { .. }
        | ClientMsg::UpgradeBuilding { .. }
        | ClientMsg::TrainUnits { .. }
        | ClientMsg::CancelTraining { .. }
        | ClientMsg::Trade { .. }) => {
            let mut world = state.world.lock().unwrap();
            if let Err(text) = apply_command(&mut world, &state.ticks, player_id, msg) {
                let _ = out_tx.try_send(ServerMsg::Rejected { text });
            }
        }
        ClientMsg::AdminLogin { token } => {
            *is_admin = hub.admin.check(&token).await;
            let _ = out_tx.send(ServerMsg::AdminStatus { ok: *is_admin }).await;
        }
        ClientMsg::AdminGive { goods, taler } => {
            if *is_admin {
                state
                    .world
                    .lock()
                    .unwrap()
                    .admin_give(player_id, &goods, taler);
            } else {
                let _ = out_tx.try_send(ServerMsg::Rejected {
                    text: protocol::Text::new("admin.denied"),
                });
            }
        }
        ClientMsg::AdminGameSpeed { percent } => {
            if *is_admin {
                // 0 = Pause, sonst 25–800 %.
                let percent = if percent == 0 {
                    0
                } else {
                    percent.clamp(25, 800)
                };
                state.game_speed.store(percent, Ordering::Relaxed);
                let _ = state.ticks.send(ServerMsg::GameSpeed { percent });
                info!("Spieltempo {percent} %");
            } else {
                let _ = out_tx.try_send(ServerMsg::Rejected {
                    text: protocol::Text::new("admin.denied"),
                });
            }
        }
    }
}

/// Spielbefehl eines Spielers auf die Welt anwenden und die Änderungen an alle melden.
/// Menschen (über den WebSocket) und Computergegner (`ai`) gehen beide hier durch,
/// so gelten für alle dieselben Regeln. Andere Nachrichten tun hier nichts.
pub fn apply_command(
    world: &mut World,
    ticks: &broadcast::Sender<ServerMsg>,
    player_id: u32,
    msg: ClientMsg,
) -> Result<(), protocol::Text> {
    // Alles noch unter dem Lock verschicken, damit die Reihenfolge stimmt.
    let send = |msg: ServerMsg| drop(ticks.send(msg));
    match msg {
        ClientMsg::MoveUnits { unit_ids, x, y } => {
            world.order_move(player_id, &unit_ids, (x, y));
        }
        ClientMsg::PlaceBuilding {
            kind,
            x,
            y,
            rotation,
            level,
        } => {
            let building = world.place_building_rotated(player_id, kind, x, y, rotation, level)?;
            // Erst die Mauerstücke, die einem Tor oder Turm weichen mussten.
            for event in world.take_events() {
                send(event);
            }
            send(ServerMsg::BuildingPlaced { building });
        }
        ClientMsg::DemolishBuilding { building_id } => {
            world.demolish(player_id, building_id)?;
            send(ServerMsg::BuildingRemoved { building_id });
        }
        ClientMsg::ConfigureBuilding {
            building_id,
            settings,
        } => {
            let building = world.configure(player_id, building_id, settings)?;
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::ReleaseTrees { building_id } => {
            world.release_trees(player_id, building_id)?;
            send(ServerMsg::BuildingRemoved { building_id });
            for event in world.take_events() {
                send(event);
            }
        }
        ClientMsg::MoveBuilding {
            building_id,
            x,
            y,
            rotation,
        } => {
            let building = world.move_building(player_id, building_id, x, y, rotation)?;
            // Für die Clients: alt weg, neu da (Belegung, Anschlüsse der Mauern).
            send(ServerMsg::BuildingRemoved { building_id });
            send(ServerMsg::BuildingPlaced { building });
        }
        ClientMsg::UpgradeBuilding { building_id } => {
            let building = world.upgrade(player_id, building_id)?;
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::TrainUnits {
            building_id,
            kind,
            count,
            spread,
        } => {
            let building = if spread {
                world.train_spread(player_id, building_id, kind, count)?
            } else {
                world.train(player_id, building_id, kind, count)?
            };
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::CancelTraining { building_id } => {
            let building = world.cancel_training(player_id, building_id)?;
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::Trade { good, amount, sell } => world.trade(player_id, good, amount, sell)?,
        _ => {}
    }
    Ok(())
}
