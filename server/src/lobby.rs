//! Lobby: Spieler, wartende und laufende Spiele.
//!
//! - Jeder Browser hat einen geheimen Token (erzeugt er selbst, liegt im
//!   `localStorage`). Daraus leitet der Server eine öffentliche Id ab, die
//!   andere sehen dürfen. Ein Konto mit Passwort gibt es (noch) nicht.
//! - WebSocket `/lobby`: Spiele anlegen, beitreten, Startplatz wählen, starten.
//! - Jeder sitzt höchstens in einem wartenden Spiel, kann aber in beliebig vielen
//!   laufenden mitspielen. Ein laufendes Spiel ohne verbundenen Menschen steht still.
//! - Ein laufendes Spiel nimmt niemanden mehr auf; nur wer dort einen
//!   Startplatz hat, kommt (wieder) hinein (siehe `game.rs`).
//! - Laufende Spiele werden in `SAVES_DIR` (Standard `server/data/games/`)
//!   gespeichert und nach einem Neustart des Servers fortgesetzt. Wartende
//!   Spiele gehen beim Neustart verloren.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use protocol::{
    AiLevel, AiPersonalities, GamePhase, GameSummary, LobbyClientMsg, LobbyGame, LobbyMember, LobbyServerMsg,
    LobbySlot, Text, TradeMode,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    ai,
    admin::Admin,
    economy::Economy,
    game::{self, Match},
    map::GameMap,
    maps_api::MapStore,
    preview,
    world::{SaveGame, World},
};

/// Längster Spielername.
const MAX_NAME: usize = 24;

#[derive(Clone, Serialize, Deserialize)]
pub struct Member {
    pub token: String,
    pub name: String,
    /// Computergegner: seine Stärke. Sein Token ist kein gültiger Browser-Token
    /// (siehe `computer_token`), also kann sich niemand damit anmelden.
    #[serde(default)]
    pub computer: Option<AiLevel>,
    /// Computergegner: Id seines Charakters (leer = der erste).
    #[serde(default)]
    pub personality: String,
    /// Bündnis (gleiche Zahl ab 1 = verbündet), 0 = keins.
    #[serde(default)]
    pub team: u8,
    /// Computergegner: eigene Regler (0 bis 100), sonst die seines Charakters.
    #[serde(default)]
    pub daring: Option<u8>,
    #[serde(default)]
    pub helpfulness: Option<u8>,
}

impl Member {
    fn human(token: &str, name: &str) -> Member {
        Member {
            token: token.into(),
            name: name.into(),
            computer: None,
            personality: String::new(),
            team: 0,
            daring: None,
            helpfulness: None,
        }
    }

    pub fn is_human(&self) -> bool {
        self.computer.is_none()
    }
}

/// Token eines Computergegners: enthält `:`, das kein Browser-Token enthalten darf.
fn computer_token(game: u32, n: u64) -> String {
    format!("computer:{game}:{n}")
}

/// Was sich an einem Spiel ändert (wer wo sitzt, Phase).
pub struct Meta {
    pub map_name: String,
    pub map_text: String,
    pub width: u32,
    pub height: u32,
    pub spawns: Vec<(u32, u32)>,
    pub phase: GamePhase,
    pub host: String,
    /// Je Startplatz; die Spieler-Nummer im Spiel ist Index + 1.
    pub slots: Vec<Option<Member>>,
    /// Handel unter Verbündeten: sofort oder mit Karren.
    pub trade_mode: TradeMode,
    /// Fernkampf: Wurfbahn oder Sichtlinie.
    pub shot_mode: protocol::ShotMode,
    /// Kartenvorschau für die Lobby, solange das Spiel wartet.
    pub preview: Arc<protocol::GamePreview>,
}

impl Meta {
    fn slot_of(&self, token: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|m| m.token == token))
    }

    fn members(&self) -> impl Iterator<Item = &Member> {
        self.slots.iter().flatten()
    }

    /// Nur die Menschen (ohne Computergegner).
    fn humans(&self) -> impl Iterator<Item = &Member> {
        self.members().filter(|m| m.is_human())
    }

    /// Computergegner mit ihrer Spieler-Nummer (Startplatz + 1).
    pub fn computers(&self) -> Vec<ai::Seat> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let m = s.as_ref()?;
                Some(ai::Seat {
                    player: i as u32 + 1,
                    level: m.computer?,
                    personality: m.personality.clone(),
                    daring: m.daring,
                    helpfulness: m.helpfulness,
                })
            })
            .collect()
    }
}

pub struct Game {
    pub id: u32,
    pub meta: Mutex<Meta>,
    /// Gesetzt, sobald das Spiel gestartet ist.
    pub running: OnceLock<Arc<Match>>,
}

impl Game {
    /// Vorschau der gewählten Karte (wartendes Spiel).
    pub fn waiting_preview(&self) -> Option<Arc<protocol::GamePreview>> {
        let meta = self.meta.lock().unwrap();
        (meta.phase == GamePhase::Waiting).then(|| meta.preview.clone())
    }
}

/// Datei eines laufenden Spiels (`SAVES_DIR/<id>.json`).
#[derive(Serialize, Deserialize)]
struct GameFile {
    id: u32,
    map: String,
    /// Die Karte, wie sie beim Start war (die Datei im Kartenordner kann sich ändern).
    map_text: String,
    host: String,
    slots: Vec<Option<Member>>,
    speed: u32,
    save: Option<SaveGame>,
}

/// Ein festgehaltener Spielstand, der noch in die Datei muss.
pub struct PendingSave {
    file: GameFile,
    /// Laufende Nummer: ein älterer Stand überschreibt nie einen neueren.
    seq: u64,
    dir: PathBuf,
    path: PathBuf,
    state: Arc<Match>,
}

impl PendingSave {
    pub fn write(self) -> Result<(), String> {
        let mut written = self.state.saved_seq.lock().unwrap();
        if *written > self.seq {
            return Ok(());
        }
        let json = serde_json::to_string(&self.file).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        *written = self.seq;
        Ok(())
    }
}

struct LobbyConn {
    token: String,
    tx: tokio::sync::mpsc::Sender<LobbyServerMsg>,
}

pub struct Hub {
    /// Reihenfolge beim Sperren: `games`, dann `Game::meta`, dann Welt/Clients
    /// eines Spiels; `conns` immer allein.
    pub games: Mutex<BTreeMap<u32, Arc<Game>>>,
    conns: Mutex<HashMap<u64, LobbyConn>>,
    next_game: AtomicU32,
    next_conn: AtomicU64,
    pub store: Arc<MapStore>,
    pub admin: Admin,
    /// Wirtschaftsdaten für neue Spiele (laufende haben ihre eigene Kopie).
    pub economy: Mutex<Economy>,
    /// Charaktere der Computergegner (Editor `/ai.html`).
    pub personalities: Mutex<AiPersonalities>,
    pub saves_dir: PathBuf,
}

/// Öffentliche Id zu einem Token (der Token selbst bleibt geheim).
pub fn public_id(token: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "uk-player".hash(&mut h);
    token.hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

fn valid_token(token: &str) -> bool {
    (16..=64).contains(&token.len()) && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn clean_name(name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    if name.is_empty() {
        "?".into()
    } else {
        name
    }
}

type Reply = Result<(), Text>;

/// Platz im Spiel: Spieler-Nummer und Startfeld.
pub type Seat = (u32, (u32, u32));

fn err(key: &str) -> Reply {
    Err(Text::new(key))
}

impl Hub {
    pub fn new(store: Arc<MapStore>, admin: Admin, economy: Economy, saves_dir: PathBuf) -> Hub {
        Hub {
            games: Mutex::new(BTreeMap::new()),
            conns: Mutex::new(HashMap::new()),
            next_game: AtomicU32::new(1),
            next_conn: AtomicU64::new(1),
            store,
            admin,
            economy: Mutex::new(economy),
            personalities: Mutex::new(ai::personality::parse(ai::personality::PERSONALITIES_JSON).expect("eingebaute Charaktere sind gültig")),
            saves_dir,
        }
    }

    pub fn game(&self, id: u32) -> Option<Arc<Game>> {
        self.games.lock().unwrap().get(&id).cloned()
    }

    /// Ein Spiel, in dem dieser Token einen Platz hat (das wartende zuerst).
    pub fn game_of(&self, token: &str) -> Option<Arc<Game>> {
        self.waiting_game_of(token).or_else(|| {
            let games: Vec<Arc<Game>> = self.games.lock().unwrap().values().cloned().collect();
            games.into_iter().find(|g| g.meta.lock().unwrap().slot_of(token).is_some())
        })
    }

    /// Das wartende Spiel, in dem dieser Token sitzt (davon gibt es höchstens eins).
    pub fn waiting_game_of(&self, token: &str) -> Option<Arc<Game>> {
        let games: Vec<Arc<Game>> = self.games.lock().unwrap().values().cloned().collect();
        games.into_iter().find(|g| {
            let meta = g.meta.lock().unwrap();
            meta.phase == GamePhase::Waiting && meta.slot_of(token).is_some()
        })
    }

    /// Laufendes Spiel, in dem der Token mitspielt: Spiel, Zustand, (Spieler-Nummer, Startfeld).
    pub fn membership(&self, id: u32, token: &str) -> Option<(Arc<Game>, Arc<Match>, Seat)> {
        let game = self.game(id)?;
        let running = game.running.get()?.clone();
        let seat = {
            let meta = game.meta.lock().unwrap();
            let slot = meta.slot_of(token)?;
            if meta.slots[slot].as_ref().is_some_and(|m| !m.is_human()) {
                return None;
            }
            (slot as u32 + 1, meta.spawns[slot])
        };
        Some((game, running, seat))
    }

    // ---------- Speichern ----------

    fn save_path(&self, id: u32) -> PathBuf {
        self.saves_dir.join(format!("{id}.json"))
    }

    /// Laufendes Spiel in seine Datei schreiben (erst `.tmp`, dann umbenennen).
    pub fn save_game(&self, game: &Game) -> Result<(), String> {
        match self.prepare_save(game) {
            Some(save) => save.write(),
            None => Ok(()),
        }
    }

    /// Den Stand eines Spiels festhalten (kurz unter der Sperre der Welt). Das teure
    /// Umwandeln in JSON und Schreiben macht `PendingSave::write`, gern in einem anderen Thread.
    pub fn prepare_save(&self, game: &Game) -> Option<PendingSave> {
        let m = game.running.get()?;
        let (host, slots) = {
            let meta = game.meta.lock().unwrap();
            (meta.host.clone(), meta.slots.clone())
        };
        let save = m.world.lock().unwrap().to_save(&m.map_name);
        let seq = m.save_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let file = GameFile {
            id: game.id,
            map: m.map_name.clone(),
            map_text: m.map_text.clone(),
            host,
            slots,
            speed: m.game_speed.load(Ordering::Relaxed),
            save: Some(save),
        };
        Some(PendingSave { file, seq, dir: self.saves_dir.clone(), path: self.save_path(game.id), state: m.clone() })
    }

    /// Alle laufenden Spiele speichern (beim Herunterfahren des Servers).
    pub fn save_all(&self) {
        let games: Vec<Arc<Game>> = self.games.lock().unwrap().values().cloned().collect();
        for game in games {
            match self.save_game(&game) {
                Ok(()) => info!("Spiel {} gespeichert", game.id),
                Err(e) => warn!(
                    "Spiel {}: Speichern beim Beenden fehlgeschlagen: {e}",
                    game.id
                ),
            }
        }
    }

    /// Welt aus der Spieldatei neu laden (Admin: "Laden").
    pub fn load_world(&self, id: u32) -> Result<World, String> {
        let path = self.save_path(id);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: GameFile =
            serde_json::from_str(&text).map_err(|e| format!("Spielstand unlesbar: {e}"))?;
        let map = GameMap::parse(&file.map_text).map_err(|e| e.to_string())?;
        World::from_save(
            map,
            self.economy.lock().unwrap().clone(),
            file.save.ok_or("kein Spielstand")?,
        )
    }

    /// Beim Serverstart: alle gespeicherten Spiele weiterlaufen lassen.
    pub fn restore(self: &Arc<Hub>) {
        let Ok(dir) = std::fs::read_dir(&self.saves_dir) else {
            return;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let loaded = (|| -> Result<(GameFile, World, GameMap), String> {
                let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let mut file: GameFile =
                    serde_json::from_str(&text).map_err(|e| format!("unlesbar: {e}"))?;
                let map = GameMap::parse(&file.map_text).map_err(|e| e.to_string())?;
                let world_map = GameMap::parse(&file.map_text).map_err(|e| e.to_string())?;
                let save = file.save.take().ok_or("kein Spielstand")?;
                let world =
                    World::from_save(world_map, self.economy.lock().unwrap().clone(), save)?;
                Ok((file, world, map))
            })();
            match loaded {
                Ok((file, world, map)) => {
                    let id = file.id;
                    match self.launch(file, world, map) {
                        Ok(_) => info!("Spiel {id} aus {} fortgesetzt", path.display()),
                        Err(e) => warn!("{}: {e}, übersprungen", path.display()),
                    }
                }
                Err(e) => warn!("Spiel {} nicht geladen: {e}", path.display()),
            }
        }
    }

    /// Ein laufendes Spiel aus Datei-Inhalt und Welt aufsetzen und seinen Thread starten.
    fn launch(self: &Arc<Hub>, file: GameFile, world: World, map: GameMap) -> Result<Arc<Game>, String> {
        if file.slots.len() != map.spawns.len() {
            return Err("Startplätze passen nicht zur Karte".into());
        }
        let meta = Meta {
            map_name: file.map.clone(),
            map_text: file.map_text.clone(),
            width: map.width,
            height: map.height,
            spawns: map.spawns.clone(),
            phase: GamePhase::Running,
            host: file.host,
            slots: file.slots,
            trade_mode: world.trade_mode(),
            shot_mode: world.shot_mode(),
            preview: Arc::new(preview::of_map(&map)),
        };
        let computers = meta.computers();
        let game = Arc::new(Game {
            id: file.id,
            meta: Mutex::new(meta),
            running: OnceLock::new(),
        });
        let mut m = Match::new(world, file.map, file.map_text, file.speed);
        m.set_computers(&computers, &self.personalities.lock().unwrap());
        let m = Arc::new(m);
        let _ = game.running.set(m.clone());
        self.games.lock().unwrap().insert(file.id, game.clone());
        self.next_game.fetch_max(file.id + 1, Ordering::Relaxed);
        game::spawn_thread(self.clone(), game.clone(), m);
        Ok(game)
    }

    /// Admin: ein laufendes Spiel samt aktuellem Spielstand als neues Spiel kopieren.
    /// Gleiche Karte, gleiche Mitspieler und Computergegner, gleicher Gastgeber;
    /// danach laufen beide Spiele unabhängig weiter. Gibt die Id der Kopie zurück.
    pub fn duplicate(self: &Arc<Hub>, id: u32) -> Result<u32, String> {
        let game = self.game(id).ok_or("admin.no_game")?;
        let pending = self.prepare_save(&game).ok_or("admin.no_game")?;
        let new_id = self.start_from_file(pending.file)?;
        info!("Spiel {id} kopiert als Spiel {new_id}");
        Ok(new_id)
    }

    /// Admin: aktueller Stand eines laufenden Spiels als Datei (gleiches Format wie in `SAVES_DIR`).
    pub fn export(&self, id: u32) -> Result<String, String> {
        let game = self.game(id).ok_or("admin.no_game")?;
        let pending = self.prepare_save(&game).ok_or("admin.no_game")?;
        serde_json::to_string(&pending.file).map_err(|e| e.to_string())
    }

    /// Admin: eine heruntergeladene Spieldatei als neues laufendes Spiel starten.
    pub fn import(self: &Arc<Hub>, text: &str) -> Result<u32, String> {
        let file: GameFile = serde_json::from_str(text).map_err(|_| "admin.bad_game_file".to_string())?;
        let old = file.id;
        let new_id = self.start_from_file(file)?;
        info!("Spieldatei (Spiel {old}) geladen als Spiel {new_id}");
        Ok(new_id)
    }

    /// Spieldatei unter neuer Id starten und gleich speichern (übersteht so einen Neustart).
    fn start_from_file(self: &Arc<Hub>, mut file: GameFile) -> Result<u32, String> {
        let save = file.save.take().ok_or("admin.bad_game_file")?;
        let map = GameMap::parse(&file.map_text).map_err(|_| "admin.bad_game_file")?;
        let world_map = GameMap::parse(&file.map_text).map_err(|_| "admin.bad_game_file")?;
        let world = World::from_save(world_map, self.economy.lock().unwrap().clone(), save)?;
        let new_id = self.next_game.fetch_add(1, Ordering::Relaxed);
        file.id = new_id;
        let game = self.launch(file, world, map)?;
        if let Err(e) = self.save_game(&game) {
            warn!("Spiel {new_id}: Speichern fehlgeschlagen: {e}");
        }
        self.notify();
        Ok(new_id)
    }

    /// Spiel ganz beenden: Thread anhalten, Clients raus, Datei weg.
    pub fn end_game(&self, id: u32) {
        let Some(game) = self.games.lock().unwrap().remove(&id) else {
            return;
        };
        if let Some(m) = game.running.get() {
            m.stop.store(true, Ordering::Relaxed);
            m.close_all();
            let _ = std::fs::remove_file(self.save_path(id));
        }
        info!("Spiel {id} entfernt");
    }

    // ---------- Lobby-Stand an alle ----------

    fn online_tokens(&self) -> HashSet<String> {
        self.conns
            .lock()
            .unwrap()
            .values()
            .map(|c| c.token.clone())
            .collect()
    }

    /// Allen in der Lobby den neuen Stand schicken (jedem seinen eigenen).
    pub fn notify(&self) {
        let lobby_online = self.online_tokens();
        let games: Vec<Arc<Game>> = self.games.lock().unwrap().values().cloned().collect();
        let mut summaries = Vec::new();
        // Pro Spiel: Detailansicht und welche Tokens dazugehören.
        let mut details: Vec<(LobbyGame, Vec<String>)> = Vec::new();
        for g in &games {
            let meta = g.meta.lock().unwrap();
            let in_game = g
                .running
                .get()
                .map(|m| m.online_players())
                .unwrap_or_default();
            let paused = g.running.get().is_some_and(|m| m.paused());
            // Reihenfolge: erst `meta`, dann die Welt (siehe `Hub::games`).
            let defeated: Vec<u32> = g
                .running
                .get()
                .map(|m| m.world.lock().unwrap().eliminated())
                .unwrap_or_default();
            let host_name = meta
                .members()
                .find(|m| m.token == meta.host)
                .map(|m| m.name.clone())
                .unwrap_or_default();
            let slots: Vec<LobbySlot> = meta
                .slots
                .iter()
                .enumerate()
                .map(|(i, s)| LobbySlot {
                    x: meta.spawns[i].0,
                    y: meta.spawns[i].1,
                    player_id: i as u32 + 1,
                    member: s.as_ref().map(|m| LobbyMember {
                        id: public_id(&m.token),
                        name: m.name.clone(),
                        online: !m.is_human()
                            || lobby_online.contains(&m.token)
                            || in_game.contains(&(i as u32 + 1)),
                        computer: m.computer,
                        personality: m.computer.map(|_| m.personality.clone()),
                        team: m.team,
                        daring: m.daring,
                        helpfulness: m.helpfulness,
                        defeated: defeated.contains(&(i as u32 + 1)),
                    }),
                })
                .collect();
            summaries.push(GameSummary {
                id: g.id,
                map: meta.map_name.clone(),
                phase: meta.phase,
                host_name,
                players: meta.members().count() as u32,
                max_players: meta.slots.len() as u32,
                mine: false,
                paused,
                slots: slots.clone(),
            });
            let detail = LobbyGame {
                id: g.id,
                map: meta.map_name.clone(),
                width: meta.width,
                height: meta.height,
                phase: meta.phase,
                host: public_id(&meta.host),
                slots,
                trade_mode: meta.trade_mode,
                shot_mode: meta.shot_mode,
            };
            details.push((detail, meta.members().map(|m| m.token.clone()).collect()));
        }
        let conns = self.conns.lock().unwrap();
        for c in conns.values() {
            let mut games = summaries.clone();
            for (s, (_, tokens)) in games.iter_mut().zip(&details) {
                s.mine = tokens.contains(&c.token);
            }
            // Der Raum: nur das wartende Spiel, laufende stehen in der Liste.
            let current = details
                .iter()
                .find(|(d, tokens)| d.phase == GamePhase::Waiting && tokens.contains(&c.token))
                .map(|(d, _)| d.clone());
            let _ = c.tx.try_send(LobbyServerMsg::State { games, current });
        }
    }

    // ---------- Aktionen ----------

    fn set_name(&self, token: &str, name: &str) {
        let games: Vec<Arc<Game>> = self.games.lock().unwrap().values().cloned().collect();
        for g in games {
            let mut meta = g.meta.lock().unwrap();
            for m in meta.slots.iter_mut().flatten() {
                if m.token == token {
                    m.name = name.to_string();
                }
            }
        }
    }

    /// Karte lesen: (Text, Karte). Nur Karten mit mindestens einem Startplatz.
    fn read_map(&self, name: &str) -> Result<(String, GameMap), Text> {
        let text = self
            .store
            .read(name)
            .ok_or_else(|| Text::new("lobby.map_missing").arg("map", name))?;
        let map =
            GameMap::parse(&text).map_err(|_| Text::new("lobby.map_invalid").arg("map", name))?;
        if map.spawns.is_empty() {
            return Err(Text::new("lobby.map_no_spawns").arg("map", name));
        }
        Ok((text, map))
    }

    fn create(&self, token: &str, name: &str, map_name: &str) -> Reply {
        if self.waiting_game_of(token).is_some() {
            return err("lobby.already_in_game");
        }
        let (text, map) = self.read_map(map_name)?;
        let id = self.next_game.fetch_add(1, Ordering::Relaxed);
        let mut slots = vec![None; map.spawns.len()];
        slots[0] = Some(Member::human(token, name));
        let meta = Meta {
            map_name: map_name.into(),
            map_text: text,
            width: map.width,
            height: map.height,
            spawns: map.spawns.clone(),
            phase: GamePhase::Waiting,
            host: token.into(),
            slots,
            trade_mode: TradeMode::default(),
            shot_mode: protocol::ShotMode::default(),
            preview: Arc::new(preview::of_map(&map)),
        };
        let game = Arc::new(Game {
            id,
            meta: Mutex::new(meta),
            running: OnceLock::new(),
        });
        self.games.lock().unwrap().insert(id, game);
        info!("Spiel {id} angelegt ({map_name})");
        Ok(())
    }

    fn join(&self, token: &str, name: &str, id: u32) -> Reply {
        if self.waiting_game_of(token).is_some() {
            return err("lobby.already_in_game");
        }
        let Some(game) = self.game(id) else {
            return err("lobby.game_gone");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let Some(free) = meta.slots.iter().position(|s| s.is_none()) else {
            return err("lobby.game_full");
        };
        meta.slots[free] = Some(Member::human(token, name));
        Ok(())
    }

    /// Spiel verlassen: das mit dieser Id, ohne Id das wartende (sonst irgendeins).
    fn leave(&self, token: &str, id: Option<u32>) -> Reply {
        let game = match id {
            Some(id) => self.game(id).filter(|g| g.meta.lock().unwrap().slot_of(token).is_some()),
            None => self.game_of(token),
        };
        let Some(game) = game else {
            return Ok(());
        };
        let empty = {
            let mut meta = game.meta.lock().unwrap();
            if let Some(i) = meta.slot_of(token) {
                meta.slots[i] = None;
            }
            // Computergegner halten kein Spiel am Leben und werden nie Gastgeber.
            let next = meta.humans().next().map(|m| m.token.clone());
            if meta.host == token {
                if let Some(next) = &next {
                    meta.host = next.clone();
                }
            }
            next.is_none()
        };
        // Wer ein laufendes Spiel verlässt, verliert den Zugang; sein Reich bleibt stehen.
        if game.running.get().is_some() && !empty {
            if let Err(e) = self.save_game(&game) {
                warn!("Spiel {}: Speichern fehlgeschlagen: {e}", game.id);
            }
        }
        if empty {
            self.end_game(game.id);
        }
        Ok(())
    }

    fn take_slot(&self, token: &str, slot: u32) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let slot = slot as usize;
        if meta.slots.get(slot).is_none_or(|s| s.is_some()) {
            return err("lobby.slot_taken");
        }
        let from = meta.slot_of(token).expect("ist im Spiel");
        meta.slots[slot] = meta.slots[from].take();
        Ok(())
    }

    /// Gastgeber zieht einen Spieler (Mensch oder Computer) auf einen anderen Startplatz;
    /// sitzt dort schon jemand, tauschen beide.
    fn move_slot(&self, token: &str, from: u32, to: u32) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let (from, to) = (from as usize, to as usize);
        if from >= meta.slots.len() || to >= meta.slots.len() {
            return err("lobby.bad_slot");
        }
        if meta.slots[from].is_none() {
            return err("lobby.slot_empty");
        }
        meta.slots.swap(from, to);
        Ok(())
    }

    fn set_map(&self, token: &str, map_name: &str) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let (text, map) = self.read_map(map_name)?;
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let members: Vec<Member> = meta.members().cloned().collect();
        if members.len() > map.spawns.len() {
            return Err(Text::new("lobby.map_too_small").arg("max", map.spawns.len()));
        }
        // Wer da ist, behält seinen Platz, wenn es ihn auf der neuen Karte gibt.
        let mut slots: Vec<Option<Member>> = vec![None; map.spawns.len()];
        let mut rest = Vec::new();
        for (i, s) in meta.slots.iter().enumerate() {
            if let Some(m) = s {
                if i < slots.len() {
                    slots[i] = Some(m.clone())
                } else {
                    rest.push(m.clone())
                }
            }
        }
        for m in rest {
            let free = slots
                .iter()
                .position(|s| s.is_none())
                .expect("genug Plätze");
            slots[free] = Some(m);
        }
        meta.slots = slots;
        meta.map_name = map_name.into();
        meta.map_text = text;
        meta.width = map.width;
        meta.height = map.height;
        meta.preview = Arc::new(preview::of_map(&map));
        meta.spawns = map.spawns;
        Ok(())
    }

    /// Gastgeber setzt einen Computergegner auf einen freien Startplatz.
    fn add_computer(&self, token: &str, slot: u32, level: AiLevel, personality: Option<String>) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let slot = slot as usize;
        if meta.slots.get(slot).is_none_or(|s| s.is_some()) {
            return err("lobby.slot_taken");
        }
        meta.slots[slot] = Some(Member {
            token: computer_token(game.id, self.next_conn.fetch_add(1, Ordering::Relaxed)),
            name: format!("Computer {}", slot + 1),
            computer: Some(level),
            personality: {
                let all = self.personalities.lock().unwrap();
                ai::personality::find(&all, personality.as_deref().unwrap_or("")).id.clone()
            },
            team: 0,
            daring: None,
            helpfulness: None,
        });
        Ok(())
    }

    /// Gastgeber nimmt einen Computergegner wieder heraus.
    fn remove_computer(&self, token: &str, slot: u32) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let slot = slot as usize;
        if meta
            .slots
            .get(slot)
            .is_none_or(|s| s.as_ref().is_none_or(|m| m.is_human()))
        {
            return err("lobby.not_computer");
        }
        meta.slots[slot] = None;
        Ok(())
    }

    /// Bündnis eines Startplatzes setzen: den eigenen jeder, Computergegner nur der Gastgeber.
    fn set_team(&self, token: &str, slot: u32, team: u8) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        if team as usize > meta.slots.len() {
            return err("lobby.bad_team");
        }
        let is_host = meta.host == token;
        let Some(m) = meta.slots.get_mut(slot as usize).and_then(|s| s.as_mut()) else {
            return err("lobby.slot_empty");
        };
        if m.token != token && !(is_host && !m.is_human()) {
            return err("lobby.not_your_slot");
        }
        m.team = team;
        Ok(())
    }

    /// Gastgeber: Regler eines Computergegners für dieses Spiel (leer = wie sein Charakter).
    fn set_computer_traits(&self, token: &str, slot: u32, daring: Option<u8>, helpfulness: Option<u8>) -> Reply {
        let Some(game) = self.game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        let Some(m) = meta.slots.get_mut(slot as usize).and_then(|s| s.as_mut()).filter(|m| !m.is_human()) else {
            return err("lobby.not_computer");
        };
        m.daring = daring.map(|d| d.min(100));
        m.helpfulness = helpfulness.map(|h| h.min(100));
        Ok(())
    }

    /// Gastgeber: wie Waren zwischen Verbündeten ankommen.
    fn set_trade_mode(&self, token: &str, mode: TradeMode) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        meta.trade_mode = mode;
        Ok(())
    }

    /// Gastgeber: wann Fernkämpfer treffen (Wurfbahn oder Sichtlinie).
    fn set_shot_mode(&self, token: &str, mode: protocol::ShotMode) -> Reply {
        let Some(game) = self.game_of(token) else {
            return err("lobby.not_in_game");
        };
        let mut meta = game.meta.lock().unwrap();
        if meta.host != token {
            return err("lobby.not_host");
        }
        if meta.phase != GamePhase::Waiting {
            return err("lobby.game_running");
        }
        meta.shot_mode = mode;
        Ok(())
    }

    fn start(self: &Arc<Hub>, token: &str) -> Reply {
        let Some(game) = self.waiting_game_of(token) else {
            return err("lobby.not_in_game");
        };
        let m = {
            let mut meta = game.meta.lock().unwrap();
            if meta.host != token {
                return err("lobby.not_host");
            }
            if meta.phase != GamePhase::Waiting {
                return Ok(());
            }
            let map = GameMap::parse(&meta.map_text)
                .map_err(|_| Text::new("lobby.map_invalid").arg("map", &meta.map_name))?;
            let mut world = World::new(map, self.economy.lock().unwrap().clone());
            for (i, s) in meta.slots.iter().enumerate() {
                if let Some(m) = s {
                    world.spawn_player_at(i as u32 + 1, meta.spawns[i]);
                    world.set_team(i as u32 + 1, m.team);
                }
            }
            world.set_trade_mode(meta.trade_mode);
            world.set_shot_mode(meta.shot_mode);
            meta.phase = GamePhase::Running;
            let mut m = Match::new(world, meta.map_name.clone(), meta.map_text.clone(), 100);
            m.set_computers(&meta.computers(), &self.personalities.lock().unwrap());
            Arc::new(m)
        };
        let _ = game.running.set(m.clone());
        info!("Spiel {} gestartet", game.id);
        if let Err(e) = self.save_game(&game) {
            warn!("Spiel {}: Speichern fehlgeschlagen: {e}", game.id);
        }
        game::spawn_thread(self.clone(), game, m);
        Ok(())
    }

    fn handle(self: &Arc<Hub>, token: &str, name: &mut String, msg: LobbyClientMsg) -> Reply {
        match msg {
            LobbyClientMsg::Hello { .. } | LobbyClientMsg::Ping { .. } => Ok(()),
            LobbyClientMsg::SetName { name: n } => {
                *name = clean_name(&n);
                self.set_name(token, name);
                Ok(())
            }
            LobbyClientMsg::CreateGame { map } => self.create(token, name, &map),
            LobbyClientMsg::JoinGame { game_id } => self.join(token, name, game_id),
            LobbyClientMsg::LeaveGame { game_id } => self.leave(token, game_id),
            LobbyClientMsg::TakeSlot { slot } => self.take_slot(token, slot),
            LobbyClientMsg::MoveSlot { from, to } => self.move_slot(token, from, to),
            LobbyClientMsg::SetMap { map } => self.set_map(token, &map),
            LobbyClientMsg::StartGame => self.start(token),
            LobbyClientMsg::AddComputer { slot, level, personality } => self.add_computer(token, slot, level, personality),
            LobbyClientMsg::RemoveComputer { slot } => self.remove_computer(token, slot),
            LobbyClientMsg::SetTeam { slot, team } => self.set_team(token, slot, team),
            LobbyClientMsg::SetComputerTraits { slot, daring, helpfulness } => self.set_computer_traits(token, slot, daring, helpfulness),
            LobbyClientMsg::SetTradeMode { mode } => self.set_trade_mode(token, mode),
            LobbyClientMsg::SetShotMode { mode } => self.set_shot_mode(token, mode),
        }
    }
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(hub): State<Arc<Hub>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, hub))
}

async fn handle_socket(socket: WebSocket, hub: Arc<Hub>) {
    let (mut tx, mut rx) = socket.split();
    // Erste Nachricht muss `hello` sein.
    let (token, mut name) = loop {
        match rx.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<LobbyClientMsg>(&text) {
                Ok(LobbyClientMsg::Hello { token, name }) if valid_token(&token) => {
                    break (token, clean_name(&name))
                }
                Ok(_) | Err(_) => {
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
            },
            Some(Ok(_)) => continue,
            _ => return,
        }
    };
    let conn = hub.next_conn.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<LobbyServerMsg>(32);
    let _ = out_tx.try_send(LobbyServerMsg::Welcome {
        you: public_id(&token),
        maps: hub.store.names(),
        server_version: crate::VERSION.into(),
    });
    hub.conns.lock().unwrap().insert(
        conn,
        LobbyConn {
            token: token.clone(),
            tx: out_tx.clone(),
        },
    );
    hub.set_name(&token, &name);
    hub.admin.record_guest(&token, &name);
    hub.notify();

    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let text = serde_json::to_string(&msg).expect("serialisierbar");
            if tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(text) = msg else { continue };
        match serde_json::from_str::<LobbyClientMsg>(&text) {
            Ok(LobbyClientMsg::Ping { client_time }) => {
                let _ = out_tx.try_send(LobbyServerMsg::Pong { client_time });
            }
            Ok(msg) => {
                let renamed = matches!(msg, LobbyClientMsg::SetName { .. });
                if let Err(text) = hub.handle(&token, &mut name, msg) {
                    let _ = out_tx.try_send(LobbyServerMsg::Rejected { text });
                } else if renamed {
                    hub.admin.record_guest(&token, &name);
                }
                hub.notify();
            }
            Err(e) => warn!("Ungültige Lobby-Nachricht: {e}"),
        }
    }
    writer.abort();
    hub.conns.lock().unwrap().remove(&conn);
    hub.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> Arc<Hub> {
        let dir = std::env::temp_dir().join(format!("uk-lobby-test-{}", std::process::id()));
        let store = MapStore {
            dir: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/maps")),
            can_save: false,
        };
        let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
        Arc::new(Hub::new(Arc::new(store), Admin::for_tests(), economy, dir))
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccc";

    #[test]
    fn lobby_ablauf() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let id = hub.game_of(A).unwrap().id;
        assert!(
            hub.create(A, "Anna", "zwei-burgen").is_err(),
            "nur ein Spiel gleichzeitig"
        );
        hub.join(B, "Bert", id).unwrap();
        // Platz tauschen geht nur auf einen freien.
        assert!(hub.take_slot(B, 0).is_err());
        assert!(hub.start(B).is_err(), "nur der Gastgeber startet");
        hub.start(A).unwrap();
        let game = hub.game(id).unwrap();
        assert!(game.running.get().is_some());
        // Laufendes Spiel: niemand Neues kommt hinein, die Mitspieler schon.
        assert!(hub.join(C, "Carl", id).is_err());
        assert!(hub.membership(id, C).is_none());
        let (_, m, (pid, _)) = hub.membership(id, B).unwrap();
        assert_eq!(pid, 2);
        assert!(m.world.lock().unwrap().has_player(1));
        assert!(m.world.lock().unwrap().has_player(2));
        // Gespeichert und wieder ladbar.
        assert!(hub.load_world(id).is_ok());
        // Alle gehen: Spiel weg, Datei weg.
        hub.leave(A, None).unwrap();
        hub.leave(B, None).unwrap();
        assert!(hub.game(id).is_none());
        assert!(!hub.save_path(id).exists());
    }

    #[test]
    fn computergegner_in_der_lobby() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let id = hub.game_of(A).unwrap().id;
        hub.join(B, "Bert", id).unwrap();
        assert!(
            hub.add_computer(B, 2, AiLevel::Easy, None).is_err(),
            "nur der Gastgeber"
        );
        assert!(
            hub.add_computer(A, 1, AiLevel::Easy, None).is_err(),
            "Platz ist besetzt"
        );
        hub.add_computer(A, 2, AiLevel::Normal, None).unwrap();
        let token = {
            let game = hub.game(id).unwrap();
            let meta = game.meta.lock().unwrap();
            assert_eq!(meta.computers(), vec![ai::Seat { player: 3, level: AiLevel::Normal, personality: "balanced".into(), daring: None, helpfulness: None }]);
            meta.slots[2].as_ref().unwrap().token.clone()
        };
        assert!(
            !valid_token(&token),
            "mit dem Token kann sich kein Browser anmelden"
        );
        assert!(hub.remove_computer(A, 1).is_err(), "dort sitzt ein Mensch");
        hub.start(A).unwrap();
        assert!(hub.remove_computer(A, 2).is_err(), "läuft schon");
        let (_, m, _) = hub.membership(id, A).unwrap();
        assert!(
            m.world.lock().unwrap().has_player(3),
            "Computer hat sein Reich"
        );
        assert!(
            hub.membership(id, &token).is_none(),
            "niemand spielt als Computer"
        );
        assert_eq!(m.computers.lock().unwrap().len(), 1);
        // Bleiben nur Computer übrig, ist das Spiel aus.
        hub.leave(A, None).unwrap();
        assert!(hub.game(id).is_some());
        hub.leave(B, None).unwrap();
        assert!(hub.game(id).is_none());
    }

    #[test]
    fn buendnisse_und_handelsart_vor_dem_start() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let id = hub.game_of(A).unwrap().id;
        hub.join(B, "Bert", id).unwrap();
        hub.set_team(B, 1, 2).unwrap();
        assert!(hub.set_team(B, 0, 2).is_err(), "fremder Platz");
        assert!(hub.set_team(A, 1, 2).is_err(), "auch der Gastgeber nicht bei Menschen");
        assert!(hub.set_team(A, 0, 99).is_err(), "zu große Zahl");
        hub.set_team(A, 0, 2).unwrap();
        assert!(hub.set_trade_mode(B, TradeMode::Carts).is_err(), "nur der Gastgeber");
        hub.set_trade_mode(A, TradeMode::Carts).unwrap();
        hub.start(A).unwrap();
        assert!(hub.set_team(A, 0, 1).is_err(), "läuft schon");
        let (_, m, _) = hub.membership(id, A).unwrap();
        let w = m.world.lock().unwrap();
        assert!(w.allied(1, 2));
        assert_eq!(w.trade_mode(), TradeMode::Carts);
        drop(w);
        hub.leave(A, None).unwrap();
        hub.leave(B, None).unwrap();
    }

    #[test]
    fn mehrere_laufende_spiele_und_pause_ohne_menschen() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let first = hub.waiting_game_of(A).unwrap().id;
        hub.join(B, "Bert", first).unwrap();
        hub.start(A).unwrap();
        // Läuft das erste, darf Anna ein zweites anlegen und starten.
        assert!(hub.waiting_game_of(A).is_none());
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let second = hub.waiting_game_of(A).unwrap().id;
        assert_ne!(first, second);
        assert!(hub.create(A, "Anna", "zwei-burgen").is_err(), "nur ein wartendes Spiel");
        hub.join(C, "Carl", second).unwrap();
        hub.start(A).unwrap();
        assert!(hub.membership(first, A).is_some());
        assert!(hub.membership(second, A).is_some());
        // Niemand verbunden: beide stehen still.
        let m = hub.game(first).unwrap().running.get().unwrap().clone();
        assert!(m.paused());
        // Gezielt das erste verlassen, das zweite bleibt.
        hub.leave(A, Some(first)).unwrap();
        assert!(hub.membership(first, A).is_none());
        assert!(hub.membership(second, A).is_some());
        hub.leave(B, None).unwrap();
        hub.leave(A, None).unwrap();
        hub.leave(C, None).unwrap();
        assert!(hub.game(first).is_none() && hub.game(second).is_none());
    }

    #[test]
    fn admin_kopiert_laufendes_spiel() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let id = hub.waiting_game_of(A).unwrap().id;
        hub.join(B, "Bert", id).unwrap();
        assert!(hub.duplicate(id).is_err(), "wartendes Spiel lässt sich nicht kopieren");
        hub.start(A).unwrap();
        let copy = hub.duplicate(id).unwrap();
        assert_ne!(copy, id);
        // Gleiche Mitspieler, gleicher Stand, eigene Datei.
        assert!(hub.membership(copy, A).is_some());
        assert!(hub.membership(copy, B).is_some());
        assert!(hub.save_path(copy).exists());
        let count = |g: u32| hub.game(g).unwrap().running.get().unwrap().world.lock().unwrap().buildings.len();
        assert_eq!(count(id), count(copy));
        // Unabhängig: das Original beenden lässt die Kopie stehen.
        hub.end_game(id);
        assert!(hub.membership(copy, A).is_some());
        // Als Datei herunterladen und wieder hochladen: neues Spiel, gleicher Stand.
        let text = hub.export(copy).unwrap();
        let loaded = hub.import(&text).unwrap();
        assert!(loaded != copy && hub.membership(loaded, B).is_some());
        assert_eq!(count(copy), count(loaded));
        assert!(hub.import("kein json").is_err());
        hub.end_game(copy);
        hub.end_game(loaded);
        assert!(!hub.save_path(copy).exists());
    }

    #[test]
    fn gastgeber_verschiebt_spieler() {
        let hub = hub();
        hub.create(A, "Anna", "zwei-burgen").unwrap();
        let id = hub.game_of(A).unwrap().id;
        hub.join(B, "Bert", id).unwrap();
        hub.add_computer(A, 2, AiLevel::Easy, None).unwrap();
        let tokens = || -> Vec<Option<String>> {
            let g = hub.game(id).unwrap();
            let meta = g.meta.lock().unwrap();
            meta.slots.iter().map(|s| s.as_ref().map(|m| m.token.clone())).collect()
        };
        assert!(hub.move_slot(B, 1, 3).is_err(), "nur der Gastgeber");
        assert!(hub.move_slot(A, 3, 0).is_err(), "leerer Platz");
        assert!(hub.move_slot(A, 0, 9).is_err(), "gibt es nicht");
        // Auf einen freien Platz.
        hub.move_slot(A, 1, 3).unwrap();
        assert_eq!(tokens()[3].as_deref(), Some(B));
        assert!(tokens()[1].is_none());
        // Auf einen besetzten: tauschen (auch mit dem Computer).
        hub.move_slot(A, 0, 2).unwrap();
        let t = tokens();
        assert_eq!(t[2].as_deref(), Some(A));
        assert!(t[0].as_deref().is_some_and(|c| c.starts_with("computer:")));
        // Vorschau der wartenden Karte ist da.
        let p = hub.game(id).unwrap().waiting_preview().unwrap();
        assert_eq!((p.width, p.height), (256, 256));
        hub.start(A).unwrap();
        assert!(hub.move_slot(A, 2, 1).is_err(), "läuft schon");
        let (_, m, (pid, _)) = hub.membership(id, A).unwrap();
        assert_eq!(pid, 3);
        let w = m.world.lock().unwrap();
        let p = preview::of_world(&w, true);
        assert!(p.buildings.iter().any(|b| b.owner == 3 && b.look == protocol::PreviewLook::Keep));
        assert!(p.defeated.is_empty());
        drop(w);
        hub.leave(A, None).unwrap();
        hub.leave(B, None).unwrap();
    }

    #[test]
    fn oeffentliche_id_verraet_den_token_nicht() {
        let id = public_id(A);
        assert_eq!(id.len(), 12);
        assert_eq!(id, public_id(A));
        assert_ne!(id, public_id(B));
        assert!(!id.contains('a') || id != A);
    }
}
