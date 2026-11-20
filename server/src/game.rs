//! Ein laufendes Spiel: eigene Welt, eigener Thread mit fester Tick-Rate,
//! eigene WebSocket-Clients (`/ws?game=..&token=..`).
//!
//! Wer mitspielt, steht in der Lobby (`lobby.rs`); hier wird nur geprüft,
//! ob der Token zu einem Startplatz dieses Spiels gehört.

use std::{
    collections::{BTreeSet, HashMap},
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
use protocol::{ClientMsg, MapRef, PlayerInfo, ServerMsg, UnitInfo, UnitState};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::profile::{Phase, PhaseTimes, PHASE_COUNT, PHASE_NAMES};
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
/// So oft prüft er, ob die Burgen der verbundenen Spieler geschlossen sind.
const CASTLE_CHECK_SECONDS: u64 = 2;

/// Ein verbundener Client.
pub struct Client {
    /// Als welcher Spieler diese Verbindung spielt. Mehrere Verbindungen dürfen
    /// denselben Spieler steuern (Admin „Spielen als“: zwei bauen an einer Burg).
    player: u32,
    /// Token des Browsers: dasselbe Fenster zweimal (gleicher Token, gleicher Spieler)
    /// gibt es nicht, dann gewinnt das neue.
    token: String,
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
    /// Zuletzt geschickter Stand der eigenen Burg (geschlossen oder Lücke).
    castle: Option<protocol::CastleStatus>,
}

/// Zustand eines laufenden Spiels.
pub struct Match {
    /// Spielzustand. Nur kurz sperren, nie über ein `.await` hinweg.
    /// Reihenfolge beim Sperren: erst `world`, dann `clients`.
    pub world: Mutex<World>,
    pub map_name: String,
    /// Kartentext beim Start (für Neues Spiel und zum Speichern).
    pub map_text: String,
    /// Verbundene Clients je Verbindung (nicht je Spieler, siehe `Client::player`).
    /// Ist niemand verbunden, ist das Spiel angehalten.
    pub clients: Mutex<HashMap<u64, Client>>,
    /// Ereignisse für alle (Gebäude, Ressourcen). Ticks gehen pro Client.
    pub ticks: broadcast::Sender<ServerMsg>,
    /// Spieltempo in Prozent (100 = normal), vom Admin einstellbar.
    pub game_speed: AtomicU32,
    /// Spieler, die gerade Pause gedrückt haben. Solange einer drin ist, steht das Spiel.
    pub pauses: Mutex<BTreeSet<u32>>,
    /// Gesetzt, wenn das Spiel beendet wird: der Thread hört auf.
    pub stop: AtomicBool,
    next_conn: AtomicU64,
    /// Computergegner in diesem Spiel. Reihenfolge beim Sperren: nach `world`.
    pub(crate) computers: Mutex<Vec<Computer>>,
    /// Wie lange die letzten Ticks gedauert haben (Serverauslastung für den Admin).
    pub load: Mutex<TickLoad>,
    /// Spielstände: zuletzt vergebene Nummer und (unter dieser Sperre) zuletzt geschriebene.
    pub save_seq: AtomicU64,
    pub saved_seq: Mutex<u64>,
    /// Läuft gerade ein automatisches Speichern im Hintergrund?
    saving: Arc<AtomicBool>,
}

/// Rechenzeit der letzten Ticks eines Spiels, in Mikrosekunden.
/// Budget je Tick ist 1/TICK_RATE Sekunde (50 ms); was darüber liegt, macht das Spiel langsamer.
pub struct TickLoad {
    /// Die letzten LOAD_WINDOW Ticks, neueste hinten.
    recent: std::collections::VecDeque<u32>,
    /// Dieselben Ticks, aufgeteilt nach Teilschritten.
    phases: std::collections::VecDeque<PhaseTimes>,
    /// Ticks seit dem Start und davon zu langsame (über Budget).
    pub total: u64,
    pub late_total: u64,
}

/// Rechenzeit für Computergegner je Tick; wer danach noch dran wäre, denkt im nächsten Tick.
const AI_TICK_BUDGET: Duration = Duration::from_millis(8);

/// So viele Ticks merkt sich die Auslastung (60 s).
const LOAD_WINDOW: usize = 60 * TICK_RATE as usize;
/// Kurzer Schnitt über so viele Ticks (5 s).
const LOAD_SHORT: usize = 5 * TICK_RATE as usize;

/// Auslastung zum Anzeigen (alles in Millisekunden bzw. Prozent des Budgets).
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
pub struct LoadSummary {
    pub budget_ms: f32,
    /// Schnitt der letzten 5 Sekunden und der letzten Minute.
    pub avg_ms: f32,
    pub avg_minute_ms: f32,
    /// Längster Tick der letzten 5 Sekunden und der letzten Minute.
    pub max_ms: f32,
    pub max_minute_ms: f32,
    /// Schnitt der letzten 5 Sekunden in Prozent des Budgets.
    pub percent: f32,
    /// Ticks der letzten Minute über dem Budget.
    pub late_minute: u32,
    pub ticks_total: u64,
    pub late_total: u64,
    /// Teilschritte der letzten Minute, teuerste Spitze zuerst (nur die mit Rechenzeit).
    pub phases: Vec<PhaseLoad>,
}

/// Ein Teilschritt des Ticks in der letzten Minute (Millisekunden).
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
pub struct PhaseLoad {
    /// Schlüssel, z. B. "combat" (Client übersetzt `load.phase.<name>`); "other" = nicht zugeordnet.
    pub name: &'static str,
    pub avg_ms: f32,
    pub max_ms: f32,
    /// Anteil am langsamsten Tick der Minute.
    pub peak_ms: f32,
    /// Bei so vielen zu langsamen Ticks war dieser Teil der größte.
    pub late_lead: u32,
}

impl TickLoad {
    fn new() -> TickLoad {
        TickLoad {
            recent: std::collections::VecDeque::with_capacity(LOAD_WINDOW),
            phases: std::collections::VecDeque::with_capacity(LOAD_WINDOW),
            total: 0,
            late_total: 0,
        }
    }

    #[cfg(test)]
    pub fn record(&mut self, took: Duration) {
        self.record_phases(took, PhaseTimes::default());
    }

    pub fn record_phases(&mut self, took: Duration, phases: PhaseTimes) {
        let us = took.as_micros().min(u32::MAX as u128) as u32;
        if self.recent.len() == LOAD_WINDOW {
            self.recent.pop_front();
            self.phases.pop_front();
        }
        self.recent.push_back(us);
        self.phases.push_back(phases);
        self.total += 1;
        if took > Duration::from_secs(1) / TICK_RATE {
            self.late_total += 1;
        }
    }

    /// Verlauf der letzten Minute: Schnitt je Sekunde in Prozent des Budgets, älteste zuerst.
    pub fn history(&self) -> Vec<f32> {
        let budget_us = 1_000_000.0 / TICK_RATE as f32;
        let per = TICK_RATE as usize;
        // Nur volle Sekunden; die gerade angefangene kommt beim nächsten Abruf dazu.
        let full = self.recent.len() - self.recent.len() % per;
        let v: Vec<u32> = self.recent.iter().take(full).copied().collect();
        v.chunks(per)
            .map(|c| (c.iter().map(|&x| x as f32).sum::<f32>() / c.len() as f32 / budget_us * 1000.0).round() / 10.0)
            .collect()
    }

    pub fn summary(&self) -> LoadSummary {
        let budget_us = 1_000_000.0 / TICK_RATE as f32;
        let ms = |us: f32| (us / 10.0).round() / 100.0;
        let stats = |n: usize| {
            let part: Vec<u32> = self.recent.iter().rev().take(n).copied().collect();
            let avg = if part.is_empty() { 0.0 } else { part.iter().map(|&v| v as f32).sum::<f32>() / part.len() as f32 };
            (avg, part.iter().copied().max().unwrap_or(0) as f32)
        };
        let (avg, max) = stats(LOAD_SHORT);
        let (avg_min, max_min) = stats(LOAD_WINDOW);
        LoadSummary {
            budget_ms: ms(budget_us),
            avg_ms: ms(avg),
            avg_minute_ms: ms(avg_min),
            max_ms: ms(max),
            max_minute_ms: ms(max_min),
            percent: (avg / budget_us * 1000.0).round() / 10.0,
            late_minute: self.recent.iter().filter(|&&v| v as f32 > budget_us).count() as u32,
            ticks_total: self.total,
            late_total: self.late_total,
            phases: self.phase_summary(budget_us),
        }
    }

    /// Je Teilschritt Schnitt, Spitze und Anteil am langsamsten Tick der letzten Minute.
    /// Was keinem Teil zugeordnet ist (z. B. Warten auf Sperren), steht unter "other".
    fn phase_summary(&self, budget_us: f32) -> Vec<PhaseLoad> {
        const OTHER: usize = PHASE_COUNT;
        let n = self.recent.len();
        if n == 0 {
            return Vec::new();
        }
        let row = |total: u32, p: &PhaseTimes| {
            let mut r = [0u32; PHASE_COUNT + 1];
            r[..PHASE_COUNT].copy_from_slice(&p.0);
            r[OTHER] = total.saturating_sub(p.0.iter().sum());
            r
        };
        let mut sum = [0u64; PHASE_COUNT + 1];
        let mut max = [0u32; PHASE_COUNT + 1];
        let mut lead = [0u32; PHASE_COUNT + 1];
        let mut slowest = (0u32, [0u32; PHASE_COUNT + 1]);
        for (&total, p) in self.recent.iter().zip(self.phases.iter()) {
            let r = row(total, p);
            for i in 0..=PHASE_COUNT {
                sum[i] += r[i] as u64;
                max[i] = max[i].max(r[i]);
            }
            if total as f32 > budget_us {
                if let Some((i, _)) = r.iter().enumerate().max_by_key(|&(_, v)| *v) {
                    lead[i] += 1;
                }
            }
            if total >= slowest.0 {
                slowest = (total, r);
            }
        }
        let ms = |us: f32| (us / 10.0).round() / 100.0;
        let mut list: Vec<PhaseLoad> = (0..=PHASE_COUNT)
            .filter(|&i| max[i] > 0)
            .map(|i| PhaseLoad {
                name: if i == OTHER { "other" } else { PHASE_NAMES[i] },
                avg_ms: ms(sum[i] as f32 / n as f32),
                max_ms: ms(max[i] as f32),
                peak_ms: ms(slowest.1[i] as f32),
                late_lead: lead[i],
            })
            .collect();
        list.sort_by(|a, b| b.max_ms.total_cmp(&a.max_ms).then(b.avg_ms.total_cmp(&a.avg_ms)));
        list
    }
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
            pauses: Mutex::new(BTreeSet::new()),
            stop: AtomicBool::new(false),
            next_conn: AtomicU64::new(1),
            computers: Mutex::new(Vec::new()),
            load: Mutex::new(TickLoad::new()),
            save_seq: AtomicU64::new(0),
            saved_seq: Mutex::new(0),
            saving: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Diese Startplätze spielt der Computer (Spieler-Nummer, Stärke, Charakter).
    pub fn set_computers(&mut self, list: &[ai::Seat], personalities: &protocol::AiPersonalities) {
        let economy = self.world.get_mut().unwrap().economy.clone();
        *self.computers.get_mut().unwrap() = ai::computers(list, personalities, &economy);
    }

    pub fn is_online(&self, player: u32) -> bool {
        self.clients.lock().unwrap().values().any(|c| c.player == player)
    }

    pub fn online_players(&self) -> Vec<u32> {
        players_of(&self.clients.lock().unwrap())
    }

    /// Angehalten: kein Mensch verbunden (dann läuft die Welt nicht weiter) oder Tempo 0.
    pub fn paused(&self) -> bool {
        self.clients.lock().unwrap().is_empty() || self.effective_speed() == 0
    }

    /// Alle Clients rauswerfen (Spiel beendet): sie gehen zurück in die Lobby.
    pub fn close_all(&self) {
        for c in self.clients.lock().unwrap().values() {
            let _ = c.tx.try_send(ServerMsg::NotInGame);
        }
    }

    /// Tempo, mit dem die Welt läuft: 0, solange jemand Pause gedrückt hat.
    pub fn effective_speed(&self) -> u32 {
        if self.pauses.lock().unwrap().is_empty() {
            self.game_speed.load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Pause eines Spielers setzen oder lösen und allen melden, wenn sich etwas ändert.
    pub fn set_pause(&self, player: u32, paused: bool) {
        let players: Vec<u32> = {
            let mut p = self.pauses.lock().unwrap();
            let changed = if paused { p.insert(player) } else { p.remove(&player) };
            if !changed {
                return;
            }
            p.iter().copied().collect()
        };
        info!("Pause von Spieler {player}: {paused}");
        let _ = self.ticks.send(ServerMsg::Pauses { players });
    }

    /// Neue Welt einsetzen und alle Clients neu laden lassen.
    pub fn replace_world(&self, world: World) {
        *self.world.lock().unwrap() = world;
        let _ = self.ticks.send(ServerMsg::Reload);
    }
}

/// Spieler-Nummern der verbundenen Clients, jede nur einmal.
fn players_of(clients: &HashMap<u64, Client>) -> Vec<u32> {
    let mut players: Vec<u32> = clients.values().map(|c| c.player).collect();
    players.sort_unstable();
    players.dedup();
    players
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
        || old.hp != new.hp
        || old.on_wall != new.on_wall
        || old.inside != new.inside
        // Auf- oder abgestiegen, Pferd verletzt, Sturz: sonst sieht man es erst beim nächsten Schritt.
        || old.mounted != new.mounted
        || old.horse_hp != new.horse_hp
        || old.down.ceil() != new.down.ceil()
        || old.attack_target != new.attack_target
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
    // Seit dem letzten Speichern weitergelaufen?
    let mut dirty = false;
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
        // Kein Mensch im Spiel: anhalten, damit niemandem etwas passiert, während er weg ist.
        // Computergegner denken dann auch nicht. Einmal speichern, dann ruht alles.
        if state.clients.lock().unwrap().is_empty() {
            if dirty {
                dirty = false;
                if let Err(e) = hub.save_game(&game) {
                    warn!("Spiel {}: Speichern beim Anhalten fehlgeschlagen: {e}", game.id);
                }
            }
            continue;
        }
        dirty = true;
        tick += 1;
        // Gemessen wird alles, was der Tick tut (auch das Speichern einmal pro Minute).
        let started = Instant::now();
        let mut phases = PhaseTimes::default();
        if tick.is_multiple_of(AUTOSAVE_SECONDS * TICK_RATE as u64) && !state.saving.load(Ordering::Relaxed) {
            // Im Tick nur den Stand festhalten; JSON und Datei schreibt ein eigener Thread,
            // sonst gibt es einmal pro Minute eine Lastspitze.
            if let Some(save) = phases.time(Phase::Save, || hub.prepare_save(&game)) {
                let (saving, id) = (state.saving.clone(), game.id);
                saving.store(true, Ordering::Relaxed);
                std::thread::spawn(move || {
                    if let Err(e) = save.write() {
                        warn!("Spiel {id}: automatisches Speichern fehlgeschlagen: {e}");
                    }
                    saving.store(false, Ordering::Relaxed);
                });
            }
        }
        step(&state, tick, &mut phases);
        state.load.lock().unwrap().record_phases(started.elapsed(), phases);
    }
    info!("Spiel {} beendet", game.id);
}

fn step(state: &Match, tick: u64, phases: &mut PhaseTimes) {
    let sending;
    let mut castle_took = Duration::ZERO;
    let (units, stocks, paths, infos, castles, game_seconds) = {
        let mut world = state.world.lock().unwrap();
        let speed = state.effective_speed() as f32 / 100.0;
        let dt = speed / TICK_RATE as f32;
        let thinking = Instant::now();
        // Computergegner geben ihre Befehle wie Menschen, über denselben Weg.
        for computer in state.computers.lock().unwrap().iter_mut() {
            if world.is_eliminated(computer.player) {
                continue;
            }
            // Hat ein anderer Computergegner diesen Tick schon gut gefüllt, denkt dieser im
            // nächsten nach (50 ms später merkt niemand), statt eine Lastspitze zu machen.
            if computer.due(dt) && thinking.elapsed() > AI_TICK_BUDGET {
                computer.postpone(dt);
                continue;
            }
            // Der Computer plant seine Wirtschaft mit selbst tragenden Arbeitern.
            world.self_carry.insert(computer.player);
            for cmd in computer.think(&world, dt) {
                if let Err(text) = apply_command(&mut world, &state.ticks, computer.player, cmd) {
                    computer.rejected(&text);
                }
            }
        }
        phases.add(Phase::Ai, thinking.elapsed());
        world.phase_us = PhaseTimes::default();
        world.step(dt);
        phases.merge(&world.phase_us);
        sending = Instant::now();
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
        // Burg geschlossen? Alle zwei Sekunden für jeden verbundenen Spieler.
        let castles: HashMap<u32, protocol::CastleStatus> = if tick.is_multiple_of(CASTLE_CHECK_SECONDS * TICK_RATE as u64) {
            let players = players_of(&state.clients.lock().unwrap());
            let castle = Instant::now();
            let list = players.into_iter().map(|p| (p, world.castle_status(p))).collect();
            castle_took = castle.elapsed();
            list
        } else {
            HashMap::new()
        };
        phases.add(Phase::Castle, castle_took);
        (world.snapshot(), world.take_dirty_stocks(), paths, infos, castles, world.game_seconds())
    };
    let mut clients = state.clients.lock().unwrap();
    let players_online = players_of(&clients).len() as u32;
    for (player, stock) in stocks {
        for c in clients.values().filter(|c| c.player == player) {
            let _ = c.tx.try_send(ServerMsg::Stock { stock: stock.clone() });
        }
    }
    for client in clients.values_mut() {
        let player = client.player;
        if let Some(status) = castles.get(&player) {
            if client.castle.as_ref() != Some(status) && client.tx.try_send(ServerMsg::Castle { status: status.clone() }).is_ok() {
                client.castle = Some(status.clone());
            }
        }
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
            game_seconds,
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
    phases.add(Phase::Send, sending.elapsed().saturating_sub(castle_took));
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
/// Auslastung dieses einen Spiels für seine Spieler (nicht nur Admins): Zahlen und Verlauf.
pub async fn current_load(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<u32>,
    Query(q): Query<GameQuery>,
) -> Response {
    let token = q.token.as_deref().unwrap_or("");
    let m = match hub.membership(id, token) {
        Some((_, m, _)) => Some(m),
        None if q.play_as.is_some() && hub.admin.check(token).await => hub.game(id).and_then(|g| g.running.get().cloned()),
        None => None,
    };
    let Some(m) = m else {
        return (StatusCode::NOT_FOUND, "Spiel nicht gefunden").into_response();
    };
    let (load, history) = {
        let l = m.load.lock().unwrap();
        (l.summary(), l.history())
    };
    axum::Json(serde_json::json!({
        "paused": m.paused(),
        "units": m.world.lock().unwrap().units.len(),
        "load": load,
        "history": history,
    }))
    .into_response()
}

pub async fn current_map(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<u32>,
    Query(q): Query<GameQuery>,
) -> Response {
    let token = q.token.as_deref().unwrap_or("");
    let m = match hub.membership(id, token) {
        Some((_, m, _)) => Some(m),
        // Admin „Spielen als“ auch in fremden Spielen.
        None if q.play_as.is_some() && hub.admin.check(token).await => hub.game(id).and_then(|g| g.running.get().cloned()),
        None => None,
    };
    let Some(m) = m else {
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
    let token = q.token.clone().unwrap_or_default();
    // Admin "Spielen als": irgendein Startplatz irgendeines laufenden Spiels, auch wenn
    // der Admin dort nicht mitspielt und der Spieler gerade selbst spielt (dann bauen
    // beide an derselben Burg). Admin ist, wem der eigene Token gehört.
    let may_play_as = q.play_as.is_some() && hub.admin.check(&token).await;
    let found = match (q.game, q.play_as) {
        (Some(id), Some(pid)) if may_play_as => hub.game(id).and_then(|game| {
            let state = game.running.get()?.clone();
            let spawn = *game.meta.lock().unwrap().spawns.get((pid as usize).checked_sub(1)?)?;
            Some((game, state, (pid, spawn)))
        }),
        (_, Some(_)) => None,
        // Sonst nur, wer in diesem laufenden Spiel einen Startplatz hat.
        (Some(id), None) => hub.membership(id, &token),
        (None, None) => None,
    };
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
    // Namen der Mitspieler (Lobby), vor der Welt sperren (Reihenfolge siehe `Hub`).
    let names: Vec<(u32, String, bool)> = game
        .meta
        .lock()
        .unwrap()
        .slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|m| (i as u32 + 1, m.name.clone(), !m.is_human())))
        .collect();
    let (map, buildings, economy, stock, diplomacy, outcome, mut ticks) = {
        let mut world = state.world.lock().unwrap();
        // Erstes Mal im Spiel (oder nach "Neues Spiel"): König aufs eigene Startfeld.
        if !world.has_player(player_id) {
            world.spawn_player_at(player_id, spawn);
        }
        let client = Client {
            player: player_id,
            token: token.clone(),
            tx: out_tx.clone(),
            view: None,
            known: HashMap::new(),
            watch: None,
            watch_sent: None,
            watch_info: None,
            castle: None,
        };
        // Ist derselbe Browser als derselbe Spieler schon in einem anderen Fenster drin,
        // übernimmt dieses. Andere (Admin „Spielen als“) dürfen mitspielen.
        let mut clients = state.clients.lock().unwrap();
        for old in clients.values().filter(|c| c.player == player_id && c.token == token) {
            let _ = old.tx.try_send(ServerMsg::OpenedElsewhere);
        }
        clients.retain(|_, c| !(c.player == player_id && c.token == token));
        clients.insert(conn, client);
        drop(clients);
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
            ServerMsg::Diplomacy {
                players: names
                    .into_iter()
                    .filter(|(id, _, _)| world.has_player(*id))
                    .map(|(id, name, computer)| PlayerInfo { id, name, team: world.team_of(id), computer })
                    .collect(),
                trade_mode: world.trade_mode(),
            },
            // Ausgang bisher: wer raus ist, wer gewonnen hat (für später Verbundene).
            (world.eliminated(), world.winners().map(|w| w.to_vec())),
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
    let _ = out_tx.send(diplomacy).await;
    for player in outcome.0 {
        let _ = out_tx.send(ServerMsg::PlayerDefeated { player, by: None }).await;
    }
    if let Some(winners) = outcome.1 {
        let _ = out_tx.send(ServerMsg::GameOver { winners }).await;
    }
    let _ = out_tx
        .send(ServerMsg::GameSpeed {
            percent: state.game_speed.load(Ordering::Relaxed),
        })
        .await;
    let paused_by: Vec<u32> = state.pauses.lock().unwrap().iter().copied().collect();
    let _ = out_tx.send(ServerMsg::Pauses { players: paused_by }).await;

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
                    // Lieferungen unter Verbündeten sehen nur Absender und Empfänger.
                    Ok(ServerMsg::Shipment { from, to, .. }) if from != player_id && to != player_id => continue,
                    // Warnungen nur an den Betroffenen.
                    Ok(ServerMsg::Alert { player, .. }) if player != player_id => continue,
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
                Ok(msg) => handle_msg(&hub, &state, conn, player_id, &out_tx, &mut is_admin, msg).await,
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
    // Hatte nur diese Verbindung den Spieler, kann er seine Pause nicht mehr lösen:
    // sie endet mit der Verbindung.
    let gone = {
        let mut clients = state.clients.lock().unwrap();
        clients.remove(&conn);
        !clients.values().any(|c| c.player == player_id)
    };
    if gone {
        state.set_pause(player_id, false);
    }
    info!("Spiel {}: Spieler {player_id} getrennt", game.id);
    hub.notify();
}

async fn handle_msg(
    hub: &Hub,
    state: &Match,
    conn: u64,
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
            if let Some(c) = state.clients.lock().unwrap().get_mut(&conn) {
                c.view = Some((x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
            }
        }
        // Nicht über `apply_command`: nur der Mensch bekommt das rote X bei unerreichbarem Ziel.
        ClientMsg::MoveUnits { unit_ids, x, y, fast, append } => {
            let reachable = state
                .world
                .lock()
                .unwrap()
                .order_move_full(player_id, &unit_ids, (x, y), fast.unwrap_or(false), append.unwrap_or(false));
            if !reachable {
                drop(out_tx.try_send(ServerMsg::MoveUnreachable { x, y }));
            }
        }
        ClientMsg::RequestNavCosts => {
            let msg = {
                let world = state.world.lock().unwrap();
                let nav = &world.map.nav;
                ServerMsg::NavCosts { width: nav.width, height: nav.height, cells: nav.cost_map() }
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::WatchUnit { unit_id } => {
            if let Some(c) = state.clients.lock().unwrap().get_mut(&conn) {
                c.watch = unit_id;
                c.watch_sent = None;
                c.watch_info = None;
            }
        }
        msg @ (ClientMsg::PlaceBuilding { .. }
        | ClientMsg::PlaceBridge { .. }
        | ClientMsg::DemolishBuilding { .. }
        | ClientMsg::ConfigureBuilding { .. }
        | ClientMsg::ReleaseTrees { .. }
        | ClientMsg::MoveBuilding { .. }
        | ClientMsg::UpgradeBuilding { .. }
        | ClientMsg::TrainUnits { .. }
        | ClientMsg::CancelTraining { .. }
        | ClientMsg::PrioritizeTraining { .. }
        | ClientMsg::Attack { .. }
        | ClientMsg::Mount { .. }
        | ClientMsg::Dismount { .. }
        | ClientMsg::RepairBuilding { .. }
        | ClientMsg::Trade { .. }
        | ClientMsg::SetAutoTrade { .. }
        | ClientMsg::SetGoodLimit { .. }
        | ClientMsg::SendGoods { .. }) => {
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
        ClientMsg::SetPause { paused } => state.set_pause(player_id, paused),
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
    if world.is_eliminated(player_id) {
        return Err(protocol::Text::new("defeat.eliminated"));
    }
    match msg {
        ClientMsg::MoveUnits { unit_ids, x, y, fast, append } => {
            world.order_move_full(player_id, &unit_ids, (x, y), fast.unwrap_or(false), append.unwrap_or(false));
        }
        ClientMsg::Attack { unit_ids, target, force } => {
            world.order_attack_full(player_id, &unit_ids, target, force.unwrap_or(false));
            for event in world.take_events() {
                send(event);
            }
        }
        ClientMsg::Mount { unit_ids } => {
            world.order_mount(player_id, &unit_ids)?;
        }
        ClientMsg::Dismount { unit_ids } => {
            world.order_dismount(player_id, &unit_ids)?;
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
        ClientMsg::PlaceBridge { x, y, w, h } => {
            let building = world.place_bridge(player_id, (x, y, w, h))?;
            send(ServerMsg::BuildingPlaced { building });
        }
        ClientMsg::DemolishBuilding { building_id } => {
            world.demolish(player_id, building_id)?;
            for event in world.take_events() {
                send(event);
            }
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
        ClientMsg::RepairBuilding { building_id } => {
            let building = world.repair(player_id, building_id)?;
            send(ServerMsg::BuildingChanged { building });
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
        ClientMsg::CancelTraining { building_id, index, count } => {
            let building = match index {
                Some(index) => world.cancel_training_at(player_id, building_id, index, count.unwrap_or(1))?,
                None => world.cancel_training(player_id, building_id)?,
            };
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::PrioritizeTraining { building_id, index, count } => {
            let building = world.prioritize_training(player_id, building_id, index, count)?;
            send(ServerMsg::BuildingChanged { building });
        }
        ClientMsg::Trade { good, amount, sell } => world.trade(player_id, good, amount, sell)?,
        ClientMsg::SetAutoTrade { good, buy_below, sell_above } => world.set_auto_trade(player_id, good, buy_below, sell_above)?,
        ClientMsg::SetGoodLimit { good, stop_at, keep_in_stock } => world.set_good_limit(player_id, good, stop_at, keep_in_stock)?,
        ClientMsg::SendGoods { to, goods, taler } => {
            world.send_goods(player_id, to, &goods, taler)?;
            for event in world.take_events() {
                send(event);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auslastung_verlauf_je_sekunde() {
        let mut l = TickLoad::new();
        for _ in 0..TICK_RATE {
            l.record(Duration::from_millis(10));
        }
        for _ in 0..TICK_RATE {
            l.record(Duration::from_millis(25));
        }
        l.record(Duration::from_millis(50)); // angebrochene Sekunde zählt nicht
        assert_eq!(l.history(), vec![20.0, 50.0]);
    }

    #[test]
    fn auslastung_rechnet_schnitt_spitze_und_zu_langsame() {
        let mut l = TickLoad::new();
        for _ in 0..99 {
            l.record(Duration::from_millis(10));
        }
        l.record(Duration::from_millis(60));
        let s = l.summary();
        assert_eq!(s.budget_ms, 50.0);
        assert_eq!(s.max_ms, 60.0);
        assert_eq!(s.avg_ms, 10.5);
        assert_eq!(s.percent, 21.0);
        assert_eq!(s.late_minute, 1);
        assert_eq!((s.ticks_total, s.late_total), (100, 1));
        // Das Fenster ist begrenzt.
        for _ in 0..LOAD_WINDOW {
            l.record(Duration::from_millis(1));
        }
        assert_eq!(l.summary().late_minute, 0);
        assert_eq!(l.summary().late_total, 1);
    }

    #[test]
    fn auslastung_nennt_die_teilschritte_der_spitze() {
        let mut l = TickLoad::new();
        let mut calm = PhaseTimes::default();
        calm.add(Phase::Combat, Duration::from_millis(2));
        for _ in 0..10 {
            l.record_phases(Duration::from_millis(3), calm);
        }
        let mut spike = PhaseTimes::default();
        spike.add(Phase::Ai, Duration::from_millis(55));
        spike.add(Phase::Combat, Duration::from_millis(3));
        l.record_phases(Duration::from_millis(60), spike);
        let phases = l.summary().phases;
        assert_eq!(phases[0].name, "ai");
        assert_eq!((phases[0].max_ms, phases[0].peak_ms, phases[0].late_lead), (55.0, 55.0, 1));
        assert_eq!(phases[0].avg_ms, 5.0);
        let combat = phases.iter().find(|p| p.name == "combat").unwrap();
        assert_eq!((combat.max_ms, combat.peak_ms, combat.late_lead), (3.0, 3.0, 0));
        // Was keinem Teil zugeordnet ist, steht unter "other" (hier je Tick 1 bzw. 2 ms).
        let other = phases.iter().find(|p| p.name == "other").unwrap();
        assert_eq!((other.max_ms, other.peak_ms), (2.0, 2.0));
    }

    #[test]
    fn pause_haelt_bis_alle_sie_loesen() {
        let map = crate::map::GameMap::parse(crate::map::DEFAULT_MAP).unwrap();
        let eco = crate::economy::Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
        let m = Match::new(World::new(map, eco), "t".into(), String::new(), 100);
        let mut rx = m.ticks.subscribe();
        assert_eq!(m.effective_speed(), 100);
        m.set_pause(1, true);
        m.set_pause(2, true);
        m.set_pause(2, true); // doppelt: keine neue Meldung
        assert_eq!(m.effective_speed(), 0);
        m.set_pause(1, false);
        assert_eq!(m.effective_speed(), 0, "Spieler 2 hält noch Pause");
        m.set_pause(2, false);
        assert_eq!(m.effective_speed(), 100);
        let mut seen = Vec::new();
        while let Ok(ServerMsg::Pauses { players }) = rx.try_recv() {
            seen.push(players);
        }
        assert_eq!(seen, vec![vec![1], vec![1, 2], vec![2], vec![]]);
    }
}
