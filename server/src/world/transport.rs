//! Transport: Träger und Karren bringen Waren zwischen Betrieben und Vorrat.
//!
//! Der gemeinsame Vorrat eines Spielers liegt in seinen Lagern (Bergfried,
//! Vorratslager). Jeder Betrieb hat ein kleines eigenes Lager. Zwei Aufträge:
//!
//! - **Abholen:** Fertige Waren (alles, was kein Eingang des gewählten Rezepts
//!   ist) aus dem Betrieb zum nächsten Lager bringen.
//! - **Beliefern:** Fehlende Eingänge aus dem Vorrat zum Betrieb bringen, aber
//!   (Lieferungen nimmt jeder Betrieb an, `accept_deliveries` gilt nicht mehr). Er hält
//!   Eingänge für zwei Durchläufe vorrätig.
//!
//! - **Direkt:** Braucht ein Betrieb gerade eine Ware, die ein anderer fertig hat,
//!   geht sie ohne Umweg übers Lager vom Hersteller zum Abnehmer. Das gilt auch,
//!   wenn ein Träger beim Abholen merkt, dass jemand sie braucht, und für
//!   Arbeiter, die selbst tragen (siehe `errand.rs`, `gather.rs`).
//!
//! Träger wohnen in den Lagern und tragen wenig. Der Karren aus dem
//! Wagenschuppen fährt erst ab `cart_min_load` los und lädt dann viel; er
//! lohnt sich für Betriebe, die viel herstellen oder weit weg liegen.
//! Was schon unterwegs ist, wird vorgemerkt, damit nicht zwei Träger dieselbe
//! Ladung holen.
//!
//! Bauen bezahlt man vorerst weiter direkt aus dem Vorrat.

use std::collections::{HashMap, HashSet};

use protocol::{BuildingKind, BuildingState, BuildingStatus, Good, GoodAmount, UnitKind};

use super::World;

/// So lange wartet ein Träger, bevor er wieder nach Arbeit sucht.
const IDLE_WAIT: f32 = 1.0;
/// Nach einem gescheiterten Auftrag (Ziel weg, nicht erreichbar) etwas länger.
const FAIL_WAIT: f32 = 3.0;
/// So nah (Felder vom Gebäuderand) muss ein Träger kommen, um zu laden.
const REACH: f32 = 1.6;
/// So nah am Eingangsfeld (Mitte) gilt man als „an der Tür“.
const AT_DOOR: f32 = 0.9;

/// Vorrang beim Verteilen der Aufträge: jede Sekunde Wartezeit zählt wie so viele Felder kürzerer Weg.
const WAIT_WEIGHT: f32 = 0.5;
/// Betriebe, die stillstehen (Eingang fehlt, Lager voll), kommen vor allen anderen dran.
const STALL_BONUS: f32 = 10_000.0;

/// Selbst tragende Arbeiter liefern direkt an einen Abnehmer, wenn ihr Rundweg
/// dadurch höchstens so viele Felder länger wird als über das Lager.
const DIRECT_DETOUR: f32 = 10.0;

/// Höchstens so viele Betriebe fährt ein Karren auf einer Sammelfahrt an.
const TOUR_STOPS: usize = 4;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Job {
    kind: JobKind,
    good: Good,
    amount: u32,
    stage: Stage,
    /// Sammelfahrt: weitere Betriebe (Id, Menge), die danach noch angefahren
    /// werden. Id 0 = leer.
    rest: [(u32, u32); TOUR_STOPS - 1],
}

impl Job {
    /// Für die Anzeige: (noch beim Aufladen?, Zielgebäude, Ware).
    pub(super) fn describe(&self) -> (bool, u32, Good) {
        match self.stage {
            Stage::Load(b) => (true, b, self.good),
            Stage::Unload(b) => (false, b, self.good),
        }
    }
}

/// Für Sammelfahrten: (Art, Ware) → Betriebe als (Abstand, Id, Menge).
type Groups = HashMap<(protocol::BuildingKind, Good), Vec<(f32, u32, u32)>>;

const NO_REST: [(u32, u32); TOUR_STOPS - 1] = [(0, 0); TOUR_STOPS - 1];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum JobKind {
    /// Ware aus dem Betrieb `from` in den Vorrat bringen.
    Pickup { from: u32 },
    /// Ware aus dem Vorrat zum Betrieb `to` bringen.
    Supply { to: u32 },
    /// Ware vom Betrieb `from` direkt zum Betrieb `to` bringen, der sie gerade braucht.
    Direct { from: u32, to: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Stage {
    /// Unterwegs zum Aufladen bei diesem Gebäude.
    Load(u32),
    /// Unterwegs zum Abladen bei diesem Gebäude.
    Unload(u32),
}

/// Was schon vorgemerkt ist.
#[derive(Default)]
struct Reserved {
    /// (Betrieb, Ware) → wird abgeholt.
    pickup: HashMap<(u32, Good), u32>,
    /// (Betrieb, Ware) → ist zu ihm unterwegs.
    incoming: HashMap<(u32, Good), u32>,
    /// (Spieler, Ware) → wird gleich aus dem Vorrat genommen.
    central: HashMap<(u32, Good), u32>,
}

impl Reserved {
    fn add(&mut self, owner: u32, job: &Job) {
        match (job.kind, job.stage) {
            (JobKind::Pickup { from }, Stage::Load(_)) => {
                *self.pickup.entry((from, job.good)).or_default() += job.amount;
                for &(id, n) in job.rest.iter().filter(|r| r.0 != 0) {
                    *self.pickup.entry((id, job.good)).or_default() += n;
                }
            }
            (JobKind::Supply { to }, stage) => {
                *self.incoming.entry((to, job.good)).or_default() += job.amount;
                if matches!(stage, Stage::Load(_)) {
                    *self.central.entry((owner, job.good)).or_default() += job.amount;
                }
            }
            (JobKind::Direct { from, to }, stage) => {
                *self.incoming.entry((to, job.good)).or_default() += job.amount;
                if matches!(stage, Stage::Load(_)) {
                    *self.pickup.entry((from, job.good)).or_default() += job.amount;
                }
            }
            _ => {}
        }
    }

    /// Ein Arbeiter, der selbst trägt, ist mit dieser Ladung zu `to` unterwegs.
    fn add_errand(&mut self, to: u32, load: GoodAmount) {
        *self.incoming.entry((to, load.good)).or_default() += load.amount;
    }

    fn get(map: &HashMap<(u32, Good), u32>, key: (u32, Good)) -> u32 {
        map.get(&key).copied().unwrap_or(0)
    }
}

/// Lagert dieses Lager die Ware? Ohne Auswahl lagert es alles.
fn stores(b: &BuildingState, good: Good) -> bool {
    b.settings.stores.as_ref().is_none_or(|s| s.contains(&good))
}

fn center(b: &BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

fn within(area: Option<((f32, f32), f32)>, b: &BuildingState) -> bool {
    area.is_none_or(|(c, r)| {
        let p = center(b);
        r <= 0.0 || ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() <= r
    })
}

/// Abstand eines Punktes zum Rechteck des Gebäudes (0 = darin).
fn dist_to(b: &BuildingState, p: (f32, f32)) -> f32 {
    let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
    let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

impl World {
    pub(super) fn step_transport(&mut self, dt: f32) {
        let index: HashMap<u32, usize> = self
            .buildings
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id, i))
            .collect();
        self.count_transport_wait(dt);
        let mut reserved = Reserved::default();
        for u in &self.units {
            if let Some(job) = &u.job {
                reserved.add(u.owner, job);
            }
            if let (Some(super::Gather::Deliver(to)), Some(load)) = (u.gather, u.carrying) {
                reserved.add_errand(to, load);
            }
        }
        // Ereignisgesteuert: Hat sich bei einem Spieler in diesem Takt etwas an Gebäuden
        // oder Vorrat geändert (Ergebnis fertig, Ware abgeladen, neues Gebäude ...), schauen
        // seine freien Träger sofort nach Arbeit. Sonst nur ab und zu als Rückfallebene.
        let woken: HashSet<u32> = self
            .dirty_buildings
            .iter()
            .filter_map(|id| index.get(id).map(|&i| self.buildings[i].owner))
            .chain(self.dirty_stocks.iter().copied())
            .collect();
        for k in 0..self.units.len() {
            if !self.units[k].is_transport() {
                continue;
            }
            match self.units[k].job {
                Some(job) => {
                    if self.units[k].path.is_empty() {
                        self.arrive(k, job, &index);
                    }
                }
                None => {
                    // Auch wer gerade nach Hause läuft, nimmt unterwegs neue Arbeit an.
                    let walking = !self.units[k].path.is_empty();
                    if walking && self.units[k].carrying.is_some() {
                        continue;
                    }
                    let u = &mut self.units[k];
                    u.cooldown -= dt;
                    if u.cooldown > 0.0 && !woken.contains(&u.owner) {
                        continue;
                    }
                    u.cooldown = IDLE_WAIT;
                    // Ohne Wohnung wird nicht gearbeitet.
                    let job = if self.idle_units.contains(&u.id) { None } else { self.find_job(k, &reserved, &index) };
                    if let Some(job) = job {
                        reserved.add(self.units[k].owner, &job);
                        self.start(k, job, &index);
                    } else if !walking {
                        self.go_home(k, &index);
                    }
                }
            }
        }
    }

    fn capacity(&self, kind: UnitKind) -> u32 {
        let t = &self.economy.def.transport;
        if kind == UnitKind::Cart {
            t.cart_capacity
        } else {
            t.carrier_capacity
        }
    }

    fn is_hub(&self, b: &BuildingState) -> bool {
        self.economy.building(b.kind).is_some_and(|d| d.hub)
    }

    /// Nächstes eigenes Lager zu einem Punkt, das diese Ware lagert.
    pub(super) fn nearest_hub(&self, owner: u32, p: (f32, f32), good: Good) -> Option<u32> {
        self.buildings
            .iter()
            .filter(|b| b.owner == owner && self.is_hub(b) && self.hub_accepts(b, good))
            .min_by(|a, b| dist_to(a, p).total_cmp(&dist_to(b, p)))
            .map(|b| b.id)
    }

    /// Ergebnis passt nicht mehr ins Betriebslager: wartet es aufs Abholen oder
    /// ist in keinem eigenen Lager Platz dafür?
    pub(super) fn full_status(&self, owner: u32, goods: impl IntoIterator<Item = Good>) -> BuildingStatus {
        let Some(stock) = self.stocks.get(&owner) else { return BuildingStatus::StorageFull };
        if goods.into_iter().any(|g| stock.room(g) == 0) {
            BuildingStatus::NoStorageRoom
        } else {
            BuildingStatus::StorageFull
        }
    }

    /// Liegt die Fläche (x, y, w, h) im Umkreis eines eigenen Wagenschuppens?
    /// Dann tragen neue Betriebe dort nicht mehr selbst.
    pub(super) fn cart_shed_covers(&self, owner: u32, x: u32, y: u32, w: u32, h: u32) -> bool {
        let c = (x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0);
        self.buildings.iter().filter(|b| b.owner == owner && b.kind == BuildingKind::CartShed).any(|b| {
            let r = self.economy.building(b.kind).map_or(0, |d| d.at(b.level).service_radius) as f32;
            let s = center(b);
            r <= 0.0 || (s.0 - c.0).hypot(s.1 - c.1) <= r
        })
    }

    /// Mittelpunkt und Reichweite des Wagenschuppens eines Karrens (0 = überall).
    fn service_area(&self, home: Option<u32>) -> Option<((f32, f32), f32)> {
        let b = self.buildings.iter().find(|b| Some(b.id) == home)?;
        let r = self.economy.building(b.kind).map_or(0, |d| d.at(b.level).service_radius);
        Some((center(b), r as f32))
    }

    /// Wie lange wartet jeder Betrieb schon auf Träger (Abholen oder Beliefern)?
    /// Die Uhr läuft, solange er etwas braucht, und beginnt neu, sobald ein Auftrag für ihn startet.
    fn count_transport_wait(&mut self, dt: f32) {
        let mut waiting = HashMap::new();
        for b in &self.buildings {
            let Some(inv) = self.inventories.get(&b.id) else { continue };
            let inputs: &[GoodAmount] = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).map_or(&[], |r| &r.inputs);
            let pickup = inv.state().goods.iter().any(|g| !inputs.iter().any(|i| i.good == g.good));
            let needs = pickup || matches!(b.status, BuildingStatus::MissingInput | BuildingStatus::StorageFull);
            if needs {
                waiting.insert(b.id, self.transport_wait.get(&b.id).copied().unwrap_or(0.0) + dt);
            }
        }
        self.transport_wait = waiting;
    }

    /// Vorrang eines Betriebs (in Feldern Weg): Stillstand vor allem, dann lange Wartezeit.
    fn urgency(&self, b: &BuildingState, supply: bool) -> f32 {
        let stalled = if supply {
            b.status == BuildingStatus::MissingInput
        } else {
            matches!(b.status, BuildingStatus::StorageFull | BuildingStatus::NoStorageRoom)
        };
        let wait = self.transport_wait.get(&b.id).copied().unwrap_or(0.0);
        wait * WAIT_WEIGHT + if stalled { STALL_BONUS } else { 0.0 }
    }

    /// Lager mit dieser Ware, über das der Weg Träger → Lager → Betrieb am kürzesten ist (mit Weglänge).
    fn supply_hub(&self, owner: u32, from: (f32, f32), b: &BuildingState, good: Good) -> Option<(u32, f32)> {
        self.buildings
            .iter()
            .filter(|h| h.owner == owner && self.is_hub(h) && self.hub_amount(h.id, good) > 0)
            .map(|h| (h.id, dist_to(h, from) + dist_to(b, center(h))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .or_else(|| {
                // Ware liegt noch in keinem Lager (Startwaren): beim nächsten passenden Lager abholen.
                let id = self.nearest_hub_having(owner, from, good)?;
                let h = self.buildings.iter().find(|h| h.id == id)?;
                Some((id, dist_to(h, from) + dist_to(b, center(h))))
            })
    }

    /// Bedienen Träger (`cart` = Karren) die Eingänge dieses Betriebs?
    fn served(b: &BuildingState, cart: bool) -> bool {
        if cart {
            b.settings.cart_deliveries
        } else {
            !b.settings.worker_carries
        }
    }

    /// Was gerade zu jedem Gebäude unterwegs ist: (Gebäude, Ware) → Menge.
    fn incoming_goods(&self) -> HashMap<(u32, Good), u32> {
        let mut r = Reserved::default();
        for u in &self.units {
            if let Some(job) = &u.job {
                r.add(u.owner, job);
            }
            if let (Some(super::Gather::Deliver(to)), Some(load)) = (u.gather, u.carrying) {
                r.add_errand(to, load);
            }
        }
        r.incoming
    }

    /// Wie viel von `good` braucht dieser Betrieb gerade? Eingänge für zwei Durchläufe,
    /// abzüglich dem, was er hat und was schon zu ihm unterwegs ist.
    fn direct_need(&self, bi: usize, good: Good, incoming: &HashMap<(u32, Good), u32>) -> u32 {
        let b = &self.buildings[bi];
        if b.settings.paused || self.unstaffed.contains(&b.id) || self.is_hub(b) || self.target_reached(bi) {
            return 0;
        }
        let Some(recipe) = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)) else { return 0 };
        let Some(input) = recipe.inputs.iter().find(|i| i.good == good) else { return 0 };
        let Some(inv) = self.inventories.get(&b.id) else { return 0 };
        let want = (input.amount * 2).min(inv.capacity_of(good));
        let have = inv.amount(good) + Reserved::get(incoming, (b.id, good));
        want.saturating_sub(have).min(inv.room(good))
    }

    /// Nächster eigener Betrieb (außer `except`), der `good` gerade braucht:
    /// (Id, Menge, steht er still?). `carrier`: Träger (`Some(false)`) oder Karren
    /// (`Some(true)`) beliefern nur Betriebe, die sie bedienen; Arbeiter (`None`) alle.
    #[allow(clippy::too_many_arguments)]
    fn direct_consumer(
        &self,
        owner: u32,
        good: Good,
        from: (f32, f32),
        except: u32,
        area: Option<((f32, f32), f32)>,
        carrier: Option<bool>,
        incoming: &HashMap<(u32, Good), u32>,
    ) -> Option<(u32, u32, bool)> {
        self.buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.owner == owner && b.id != except && within(area, b))
            .filter(|(_, b)| carrier.is_none_or(|cart| Self::served(b, cart)))
            .filter_map(|(bi, b)| {
                let need = self.direct_need(bi, good, incoming);
                (need > 0).then(|| (dist_to(b, from), b.id, need, b.status == BuildingStatus::MissingInput))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id, need, stalled)| (id, need, stalled))
    }

    /// Hersteller, bei dem der Weg Träger → Hersteller → Betrieb `b` am kürzesten ist und der
    /// `good` fertig hat (noch nicht vorgemerkt): (Id, Weglänge, Menge).
    fn direct_source(
        &self,
        from: (f32, f32),
        b: &BuildingState,
        good: Good,
        area: Option<((f32, f32), f32)>,
        reserved: &Reserved,
    ) -> Option<(u32, f32, u32)> {
        self.buildings
            .iter()
            .filter(|s| s.owner == b.owner && s.id != b.id && within(area, s) && !s.settings.worker_carries && !self.is_hub(s))
            .filter_map(|s| {
                let inv = self.inventories.get(&s.id)?;
                let is_input = s
                    .settings
                    .recipe
                    .as_ref()
                    .and_then(|r| self.economy.recipe(r))
                    .is_some_and(|r| r.inputs.iter().any(|i| i.good == good));
                if is_input {
                    return None;
                }
                let free = inv.amount(good).saturating_sub(Reserved::get(&reserved.pickup, (s.id, good)));
                (free > 0).then(|| (s.id, dist_to(s, from) + dist_to(b, center(s)), free))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Ein Arbeiter von Betrieb `bi`, der selbst trägt, hat `good` bei `pos`: Gibt es einen
    /// anderen Betrieb, der es gerade braucht, und lohnt sich der Weg gegenüber dem Lager `hub`?
    /// Liefert (Abnehmer, Menge, die er braucht).
    pub(super) fn worker_direct_target(&self, bi: usize, pos: (f32, f32), good: Good, hub: Option<u32>) -> Option<(u32, u32)> {
        let home = &self.buildings[bi];
        let incoming = self.incoming_goods();
        let (to, need, _) = self.direct_consumer(home.owner, good, pos, home.id, None, None, &incoming)?;
        let round = |id: u32| {
            let t = self.buildings.iter().find(|b| b.id == id)?;
            Some(dist_to(t, pos) + dist_to(home, center(t)))
        };
        let direct = round(to)?;
        let via_hub = hub.and_then(round);
        via_hub.is_none_or(|h| direct <= h + DIRECT_DETOUR).then_some((to, need))
    }

    /// Ein selbst tragender Arbeiter ist bei `target` angekommen: ins Lager (Vorrat) oder
    /// direkt in den Betrieb abladen. Was nicht passt, bleibt in seinen Händen.
    /// Liefert, ob `target` ein Lager war.
    pub(super) fn unload_errand(&mut self, k: usize, target: u32) -> bool {
        let owner = self.units[k].owner;
        let hub = self.buildings.iter().find(|b| b.id == target).is_none_or(|b| self.is_hub(b));
        let Some(load) = self.units[k].carrying.take() else { return hub };
        let fits = if hub {
            let stock = self.stocks.get_mut(&owner).expect("Spieler hat einen Vorrat");
            let fits = stock.add(load.good, load.amount);
            self.dirty_stocks.insert(owner);
            self.hub_add(target, load.good, fits);
            fits
        } else {
            let Some(inv) = self.inventories.get_mut(&target) else {
                self.units[k].carrying = Some(load);
                return hub;
            };
            let fits = inv.add(load.good, load.amount);
            self.dirty_buildings.insert(target);
            fits
        };
        if fits < load.amount {
            self.units[k].carrying = Some(GoodAmount { good: load.good, amount: load.amount - fits });
        }
        hub
    }

    /// Bester Auftrag für eine freie Einheit: der nächste, den sie erledigen kann.
    fn find_job(&self, k: usize, reserved: &Reserved, index: &HashMap<u32, usize>) -> Option<Job> {
        let u = &self.units[k];
        let owner = u.owner;
        let cap = self.capacity(u.kind);
        let cart = u.kind == UnitKind::Cart;
        let cart_min = self.economy.def.transport.cart_min_load;
        let min_load = if cart { cart_min } else { 1 };
        let stock = self.stocks.get(&owner)?;
        // Träger und Karren arbeiten nur im Umkreis ihres Schuppens
        // und nur mit den Waren, die dort ausgewählt sind.
        let area = self.service_area(u.home);
        let home = u.home.and_then(|h| index.get(&h)).map(|&i| &self.buildings[i]);
        let moves = |g: Good| home.is_none_or(|h| stores(h, g));
        // Große Ladungen lassen Träger für einen freien Karren liegen, der dort hinkommt.
        let free_carts: Vec<_> = if cart {
            Vec::new()
        } else {
            self.units
                .iter()
                .filter(|c| c.owner == owner && c.kind == UnitKind::Cart && c.job.is_none() && !self.idle_units.contains(&c.id))
                .map(|c| self.service_area(c.home))
                .collect()
        };
        let mut best: Option<(f32, Job)> = None;
        let offer = |best: &mut Option<(f32, Job)>, d: f32, job: Job| {
            if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                *best = Some((d, job));
            }
        };
        // Für Sammelfahrten: gleiche Betriebe mit derselben Ware.
        let mut groups: Groups = HashMap::new();
        for b in self.buildings.iter().filter(|b| b.owner == owner) {
            let Some(inv) = self.inventories.get(&b.id) else { continue };
            if !within(area, b) {
                continue;
            }
            // Der Arbeiter trägt hier selbst (Einstellung am Gebäude); Karren dürfen
            // trotzdem das Eingangslager auffüllen (`cart_deliveries`).
            let self_carry = b.settings.worker_carries;
            if self_carry && !(cart && b.settings.cart_deliveries) {
                continue;
            }
            let inputs: &[GoodAmount] =
                b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).map_or(&[], |r| &r.inputs);
            let d = dist_to(b, u.pos);
            // Abholen: alles, was kein Eingang ist, wenn ein Lager es nimmt.
            for g in inv.state().goods {
                if self_carry {
                    break;
                }
                if !moves(g.good) || inputs.iter().any(|i| i.good == g.good) || self.nearest_hub(owner, u.pos, g.good).is_none() {
                    continue;
                }
                let free = g.amount.saturating_sub(Reserved::get(&reserved.pickup, (b.id, g.good)));
                let room = stock.room(g.good);
                let amount = free.min(cap).min(room);
                if free >= cart_min && free_carts.iter().any(|a| within(*a, b)) {
                    continue;
                }
                if cart && amount > 0 {
                    groups.entry((b.kind, g.good)).or_default().push((d, b.id, amount));
                }
                if amount >= min_load {
                    let job = Job {
                        kind: JobKind::Pickup { from: b.id },
                        good: g.good,
                        amount,
                        stage: Stage::Load(b.id),
                        rest: NO_REST,
                    };
                    offer(&mut best, d - self.urgency(b, false), job);
                }
            }
            // Beliefern: Eingänge für zwei Durchläufe, aber nicht unter den Mindestbestand.
            if b.settings.paused || (cart && !b.settings.cart_deliveries) {
                continue;
            }
            for i in inputs.iter().filter(|i| moves(i.good)) {
                // Karren füllen das Eingangslager ganz auf, Träger bringen zwei Durchläufe.
                let want = if cart { inv.capacity_of(i.good) } else { (i.amount * 2).min(inv.capacity_of(i.good)) };
                let min_load = if cart { cart_min.min(want).max(1) } else { min_load };
                let have = inv.amount(i.good) + Reserved::get(&reserved.incoming, (b.id, i.good));
                let need = want.saturating_sub(have).min(cap);
                if need < min_load {
                    continue;
                }
                let mut choice: Option<(f32, Job)> = None;
                // Erst zum Lager, dann zum Betrieb: das Lager mit dem kürzesten Gesamtweg.
                if let Some((hub, via)) = self.supply_hub(owner, u.pos, b, i.good) {
                    let keep = b.settings.keep_in_stock.iter().find(|g| g.good == i.good).map_or(0, |g| g.amount);
                    let there = stock
                        .amount(i.good)
                        .saturating_sub(Reserved::get(&reserved.central, (owner, i.good)))
                        .saturating_sub(keep);
                    let amount = need.min(there);
                    if amount >= min_load {
                        let kind = JobKind::Supply { to: b.id };
                        choice = Some((via, Job { kind, good: i.good, amount, stage: Stage::Load(hub), rest: NO_REST }));
                    }
                }
                // Oder direkt vom Hersteller, der die Ware fertig hat, wenn der Weg nicht länger ist.
                if let Some((src, via, free)) = self.direct_source(u.pos, b, i.good, area, reserved) {
                    let amount = need.min(free);
                    if amount >= min_load && choice.as_ref().is_none_or(|(d, _)| via <= *d) {
                        let kind = JobKind::Direct { from: src, to: b.id };
                        choice = Some((via, Job { kind, good: i.good, amount, stage: Stage::Load(src), rest: NO_REST }));
                    }
                }
                if let Some((via, job)) = choice {
                    offer(&mut best, via - self.urgency(b, true), job);
                }
            }
        }
        // Karren: reicht ein Betrieb allein nicht, mehrere gleiche nacheinander anfahren.
        if cart && best.is_none() {
            for ((_, good), mut list) in groups {
                list.sort_by(|a, b| a.0.total_cmp(&b.0));
                let room = stock.room(good).min(cap);
                let mut stops: Vec<(u32, u32)> = Vec::new();
                let mut total = 0;
                for &(_, id, n) in list.iter().take(TOUR_STOPS) {
                    let n = n.min(room - total);
                    if n == 0 {
                        break;
                    }
                    stops.push((id, n));
                    total += n;
                }
                if total >= cart_min && stops.len() > 1 {
                    let mut rest = NO_REST;
                    for (slot, &stop) in rest.iter_mut().zip(&stops[1..]) {
                        *slot = stop;
                    }
                    let (first, n) = stops[0];
                    let job = Job { kind: JobKind::Pickup { from: first }, good, amount: n, stage: Stage::Load(first), rest };
                    offer(&mut best, list[0].0, job);
                }
            }
        }
        best.map(|(_, j)| j)
    }

    fn start(&mut self, k: usize, job: Job, index: &HashMap<u32, usize>) {
        let Stage::Load(at) = job.stage else { return };
        // Bedient: die Wartezeit dieses Betriebs beginnt von vorn.
        match job.kind {
            JobKind::Pickup { from: b } | JobKind::Supply { to: b } => {
                self.transport_wait.remove(&b);
            }
            JobKind::Direct { from, to } => {
                self.transport_wait.remove(&from);
                self.transport_wait.remove(&to);
            }
        }
        self.units[k].job = Some(job);
        if !self.route(k, at, index) {
            self.fail(k, index);
        }
    }

    /// Weg ist zu Ende: aufladen, abladen oder aufgeben.
    fn arrive(&mut self, k: usize, job: Job, index: &HashMap<u32, usize>) {
        let (Stage::Load(at) | Stage::Unload(at)) = job.stage;
        let Some(&bi) = index.get(&at) else {
            return self.fail(k, index);
        };
        if !self.at_door(&self.buildings[bi], self.units[k].pos) {
            return self.fail(k, index);
        }
        let owner = self.units[k].owner;
        match job.stage {
            Stage::Load(_) => {
                let taken = match job.kind {
                    JobKind::Pickup { from } | JobKind::Direct { from, .. } => {
                        let Some(inv) = self.inventories.get_mut(&from) else {
                            return self.fail(k, index);
                        };
                        let n = job.amount.min(inv.amount(job.good));
                        inv.take_all(&[GoodAmount {
                            good: job.good,
                            amount: n,
                        }]);
                        self.dirty_buildings.insert(from);
                        n
                    }
                    JobKind::Supply { .. } => {
                        let Some(stock) = self.stocks.get_mut(&owner) else {
                            return self.fail(k, index);
                        };
                        let n = job.amount.min(stock.amount(job.good));
                        stock.take_all(&[GoodAmount {
                            good: job.good,
                            amount: n,
                        }]);
                        self.dirty_stocks.insert(owner);
                        self.hub_take(at, job.good, n);
                        n
                    }
                };
                let carried = self.units[k].carrying.map_or(0, |c| c.amount) + taken;
                if carried == 0 {
                    self.units[k].job = None;
                    return;
                }
                self.units[k].carrying = Some(GoodAmount { good: job.good, amount: carried });
                // Sammelfahrt: weiter zum nächsten Betrieb.
                if let (JobKind::Pickup { .. }, (next, n)) = (job.kind, job.rest[0]) {
                    if next != 0 {
                        let mut rest = NO_REST;
                        rest[..TOUR_STOPS - 2].copy_from_slice(&job.rest[1..]);
                        self.units[k].job = Some(Job { kind: JobKind::Pickup { from: next }, amount: n, stage: Stage::Load(next), rest, ..job });
                        if self.route(k, next, index) {
                            return;
                        }
                    }
                }
                // Braucht gerade ein anderer Betrieb die abgeholte Ware? Dann direkt dorthin.
                let mut kind = job.kind;
                if let JobKind::Pickup { from } = job.kind {
                    let u = &self.units[k];
                    let incoming = self.incoming_goods();
                    let area = self.service_area(u.home);
                    let carrier = Some(u.kind == UnitKind::Cart);
                    if let Some((to, need, stalled)) = self.direct_consumer(owner, job.good, u.pos, from, area, carrier, &incoming) {
                        if need >= carried || stalled {
                            kind = JobKind::Direct { from, to };
                        }
                    }
                }
                let dest = match kind {
                    JobKind::Pickup { .. } => self.nearest_hub(owner, self.units[k].pos, job.good),
                    JobKind::Supply { to } | JobKind::Direct { to, .. } => Some(to),
                };
                let Some(dest) = dest else {
                    return self.fail(k, index);
                };
                self.units[k].job = Some(Job { kind, amount: carried, stage: Stage::Unload(dest), rest: NO_REST, ..job });
                if !self.route(k, dest, index) {
                    self.fail(k, index);
                }
            }
            Stage::Unload(_) => {
                let Some(load) = self.units[k].carrying else {
                    self.units[k].job = None;
                    return;
                };
                let left = match job.kind {
                    JobKind::Pickup { .. } => {
                        let Some(stock) = self.stocks.get_mut(&owner) else {
                            return;
                        };
                        let fits = stock.add(load.good, load.amount);
                        self.dirty_stocks.insert(owner);
                        self.hub_add(at, load.good, fits);
                        load.amount - fits
                    }
                    JobKind::Supply { to } | JobKind::Direct { to, .. } => {
                        let Some(inv) = self.inventories.get_mut(&to) else {
                            return self.fail(k, index);
                        };
                        let fits = inv.add(load.good, load.amount);
                        self.dirty_buildings.insert(to);
                        load.amount - fits
                    }
                };
                if left == 0 {
                    self.units[k].carrying = None;
                    self.units[k].job = None;
                    return;
                }
                self.units[k].carrying = Some(GoodAmount {
                    good: load.good,
                    amount: left,
                });
                if let JobKind::Supply { .. } | JobKind::Direct { .. } = job.kind {
                    // Passt nicht mehr hinein: Rest zurück ins Lager.
                    self.return_load(k, index);
                } else {
                    // Vorrat voll: warten und es gleich noch einmal versuchen.
                    self.units[k].cooldown = IDLE_WAIT;
                }
            }
        }
    }

    /// Ladung zurück zum nächsten Lager bringen.
    fn return_load(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let Some(load) = self.units[k].carrying else {
            return;
        };
        let u = &self.units[k];
        let hub = self.nearest_hub(u.owner, u.pos, load.good);
        let job = hub.map(|h| Job {
            kind: JobKind::Pickup { from: 0 },
            good: load.good,
            amount: load.amount,
            stage: Stage::Unload(h),
            rest: NO_REST,
        });
        self.units[k].job = job;
        if !hub.is_some_and(|h| self.route(k, h, index)) {
            // Kein Lager erreichbar: Ware zählt trotzdem zum Vorrat, damit nichts verloren geht.
            let owner = self.units[k].owner;
            if let Some(stock) = self.stocks.get_mut(&owner) {
                stock.add(load.good, load.amount);
                self.dirty_stocks.insert(owner);
            }
            self.units[k].carrying = None;
            self.units[k].job = None;
        }
    }

    /// Auftrag geht nicht (Ziel weg oder nicht erreichbar).
    fn fail(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let back_to_hub = matches!(
            self.units[k].job,
            Some(Job {
                stage: Stage::Load(_),
                ..
            })
        ) || matches!(
            self.units[k].job,
            Some(Job {
                kind: JobKind::Supply { .. } | JobKind::Direct { .. },
                ..
            })
        );
        self.units[k].job = None;
        self.units[k].cooldown = FAIL_WAIT;
        if self.units[k].carrying.is_some() {
            if back_to_hub {
                self.return_load(k, index);
            } else {
                // Schon auf dem Weg zum Lager gescheitert: nicht endlos im Kreis laufen.
                let u = &self.units[k];
                let (owner, load) = (u.owner, u.carrying.expect("eben geprüft"));
                if let Some(stock) = self.stocks.get_mut(&owner) {
                    stock.add(load.good, load.amount);
                    self.dirty_stocks.insert(owner);
                }
                self.units[k].carrying = None;
            }
        }
    }

    fn go_home(&mut self, k: usize, index: &HashMap<u32, usize>) {
        let Some(home) = self.units[k].home else {
            return;
        };
        let Some(&bi) = index.get(&home) else { return };
        if !self.at_door(&self.buildings[bi], self.units[k].pos) {
            self.route(k, home, index);
        }
    }

    /// Weg zum Gebäude planen, bis an ein freies Feld daneben.
    fn route(&mut self, k: usize, building: u32, index: &HashMap<u32, usize>) -> bool {
        let Some(&bi) = index.get(&building) else {
            return false;
        };
        let pos = self.units[k].pos;
        let Some(door) = self.door(&self.buildings[bi], pos) else {
            return false;
        };
        let u = &mut self.units[k];
        u.path = self.map.nav.waypoints(pos, door);
        u.target = Some(door);
        true
    }

    /// Freies Feld am Rand eines Gebäudes, das `from` am nächsten liegt.
    /// Steht die Einheit am Zugang des Gebäudes? Mit Eingang: genau dort (auch wenn
    /// Gebäude direkt nebeneinander stehen, muss man hinlaufen), sonst am Rand.
    pub(super) fn at_door(&self, b: &BuildingState, p: (f32, f32)) -> bool {
        if let Some((ex, ey)) = b.entrance {
            if self.map.nav.is_open(ex as i32, ey as i32) {
                let (dx, dy) = (ex as f32 + 0.5 - p.0, ey as f32 + 0.5 - p.1);
                return dx * dx + dy * dy <= AT_DOOR * AT_DOOR;
            }
        }
        dist_to(b, p) <= REACH
    }

    pub(super) fn door(&self, b: &BuildingState, from: (f32, f32)) -> Option<(f32, f32)> {
        // Gebäude mit Eingang: immer dorthin.
        if let Some((ex, ey)) = b.entrance {
            if self.map.nav.is_open(ex as i32, ey as i32) {
                return Some((ex as f32 + 0.5, ey as f32 + 0.5));
            }
        }
        let (x0, y0, x1, y1) = (
            b.x as i32 - 1,
            b.y as i32 - 1,
            (b.x + b.w) as i32,
            (b.y + b.h) as i32,
        );
        let ring = (x0..=x1)
            .flat_map(|x| [(x, y0), (x, y1)])
            .chain((y0 + 1..y1).flat_map(|y| [(x0, y), (x1, y)]));
        ring.filter(|&(x, y)| self.map.nav.is_open(x, y))
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .min_by(|a, b| {
                let d = |p: &(f32, f32)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
                d(a).total_cmp(&d(b))
            })
    }

    /// Neuer Träger oder Karren an einem Gebäude.
    pub(super) fn spawn_transport(&mut self, b: &BuildingState, kind: UnitKind, n: u32) {
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let Some(pos) = self.door(b, center) else {
            return;
        };
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        let mut u = super::Unit::new(id, b.owner, kind, pos);
        u.home = Some(b.id);
        // Nicht alle auf einmal losschicken.
        u.cooldown = 0.3 * n as f32;
        self.units.push(u);
    }

    /// Träger, die auf der Fläche eines neuen Gebäudes stehen, treten zur Seite.
    pub(super) fn make_room(&mut self, b: &BuildingState) {
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let door = self.door(b, center);
        for u in self.units.iter_mut().filter(|u| u.is_civilian()) {
            let inside = u.pos.0 >= b.x as f32
                && u.pos.1 >= b.y as f32
                && u.pos.0 < (b.x + b.w) as f32
                && u.pos.1 < (b.y + b.h) as f32;
            if let (true, Some(door)) = (inside, door) {
                u.pos = door;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, BuildingStatus, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::World};

    fn flat(w: usize, h: usize) -> World {
        let row = "0".repeat(w);
        let text = format!("spawn 0 {}\n{}", h - 1, format!("{row}\n").repeat(h));
        let mut world = World::new(
            GameMap::parse(&text).unwrap(),
            Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people(),
        );
        world.spawn_player(1);
        world
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as usize {
            w.step(0.05);
        }
    }

    fn central(w: &World, g: Good) -> u32 {
        w.stocks[&1].amount(g)
    }

    fn local(w: &World, id: u32, g: Good) -> u32 {
        w.inventories[&id].amount(g)
    }

    /// Wagenschuppen auf Stufe `level` (1 = Träger, 2 = dazu ein Karren).
    /// Kornspeicher: nur dort ist Platz für Obst.
    fn granary(w: &mut World, x: u32, y: u32) -> u32 {
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 50 }, GoodAmount { good: Good::Wood, amount: 50 }], 0);
        w.place_building(1, BuildingKind::Granary, x, y).unwrap().id
    }

    fn shed(w: &mut World, x: u32, y: u32, level: u8) -> u32 {
        w.place_building_rotated(1, BuildingKind::CartShed, x, y, 0, level).unwrap().id
    }

    fn count(w: &World, kind: UnitKind) -> usize {
        w.units.iter().filter(|u| u.kind == kind).count()
    }

    #[test]
    fn traeger_und_karren_nur_aus_dem_wagenschuppen() {
        let mut w = flat(30, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        w.place_building(1, BuildingKind::Storehouse, 12, 2).unwrap();
        assert_eq!(count(&w, UnitKind::Carrier), 0, "Lager bringen keine Träger mehr mit");
        let shed = shed(&mut w, 20, 2, 1);
        assert_eq!((count(&w, UnitKind::Carrier), count(&w, UnitKind::Cart)), (3, 0), "Stufe 1: Träger");
        w.upgrade(1, shed).unwrap();
        assert_eq!((count(&w, UnitKind::Carrier), count(&w, UnitKind::Cart)), (3, 1), "Stufe 2: dazu der Karren");
        let more: Vec<GoodAmount> = [Good::Wood, Good::Boards, Good::Blocks].iter().map(|&good| GoodAmount { good, amount: 200 }).collect();
        w.admin_give(1, &more, 0);
        w.upgrade(1, shed).unwrap();
        w.upgrade(1, shed).unwrap();
        let def = w.economy.building(BuildingKind::CartShed).unwrap();
        let carts: u32 = def.levels.iter().flat_map(|l| &l.spawns).filter(|sp| sp.kind == UnitKind::Cart).map(|sp| sp.count).sum();
        assert_eq!(count(&w, UnitKind::Cart) as u32, carts, "Stufe 3 und 4: weitere Karren laut Daten");
        // Träger stehen nie in Gebäuden, und befehlen lassen sie sich nicht.
        let id = w.units.iter().find(|u| u.kind == UnitKind::Carrier).unwrap().id;
        w.order_move(1, &[id], (29.5, 19.5));
        assert!(w.units.iter().find(|u| u.id == id).unwrap().path.is_empty());
        w.demolish(1, shed).unwrap();
        assert_eq!(count(&w, UnitKind::Carrier) + count(&w, UnitKind::Cart), 0);
    }

    #[test]
    fn wagenschuppen_bewegt_nur_ausgewaehlte_waren() {
        let mut w = flat(40, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        granary(&mut w, 30, 14);
        let s = shed(&mut w, 10, 12, 1);
        let mut set = w.buildings.iter().find(|b| b.id == s).unwrap().settings.clone();
        set.stores = Some(vec![Good::Wood]);
        w.configure(1, s, set).unwrap();
        let orchard = w.place_building(1, BuildingKind::Orchard, 20, 2).unwrap();
        pause(&mut w, orchard.id);
        set_fruit(&mut w, orchard.id, 4);
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 4, "Obst ist nicht ausgewählt");
        let mut set = w.buildings.iter().find(|b| b.id == s).unwrap().settings.clone();
        set.stores = None;
        w.configure(1, s, set).unwrap();
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 0, "jetzt alles");
    }

    #[test]
    fn traeger_holen_ab_und_beliefern() {
        let mut w = flat(40, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let cutter = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        shed(&mut w, 10, 12, 1);
        let wood = central(&w, Good::Wood);
        let boards = central(&w, Good::Boards);
        // Sägewerk: 2 Holz → 2 Bretter in 8 s (siehe economy.json). Ohne Lieferung fehlt Holz.
        assert_eq!(local(&w, cutter.id, Good::Wood), 0);
        run(&mut w, 60.0);
        assert!(central(&w, Good::Wood) < wood, "Holz ging zum Sägewerk");
        assert!(
            central(&w, Good::Boards) > boards,
            "Bretter kamen zurück in den Vorrat"
        );
        let b = w.buildings.iter().find(|b| b.id == cutter.id).unwrap();
        assert_ne!(b.status, BuildingStatus::StorageFull, "Bretter werden abgeholt");
    }

    #[test]
    fn stillstand_und_wartezeit_gehen_vor_kurzem_weg() {
        let mut w = flat(80, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.admin_give(1, &[GoodAmount { good: Good::Wood, amount: 100 }, GoodAmount { good: Good::Boards, amount: 50 }], 0);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        w.economy.def.buildings.iter_mut().find(|d| d.kind == BuildingKind::CartShed).unwrap().levels.iter_mut().for_each(|l| l.service_radius = 0);
        shed(&mut w, 2, 12, 1);
        let near = w.place_building(1, BuildingKind::Sawmill, 10, 2).unwrap().id;
        let far = w.place_building(1, BuildingKind::Sawmill, 70, 2).unwrap().id;
        let k = w.units.iter().position(|u| u.kind == UnitKind::Carrier).unwrap();
        let index: std::collections::HashMap<u32, usize> = w.buildings.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
        let target = |w: &World| match w.find_job(k, &super::Reserved::default(), &index).map(|j| j.kind) {
            Some(super::JobKind::Supply { to } | super::JobKind::Direct { to, .. } | super::JobKind::Pickup { from: to }) => to,
            None => 0,
        };
        let set = |w: &mut World, id: u32, st: BuildingStatus| w.buildings.iter_mut().find(|b| b.id == id).unwrap().status = st;
        // Beide brauchen Holz, keiner steht: der nähere zuerst.
        set(&mut w, near, BuildingStatus::Working);
        set(&mut w, far, BuildingStatus::Working);
        w.transport_wait.clear();
        assert_eq!(target(&w), near);
        // Der ferne wartet schon lange: er kommt vor.
        w.transport_wait.insert(far, 300.0);
        assert_eq!(target(&w), far, "lange Wartezeit");
        // Der nahe steht still (Eingang fehlt): er hat Vorrang vor allem.
        set(&mut w, near, BuildingStatus::MissingInput);
        assert_eq!(target(&w), near, "Stillstand zuerst");
    }

    #[test]
    fn traeger_kommen_sofort_wenn_etwas_fertig_ist() {
        let mut w = flat(40, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let mill = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap().id;
        w.buildings.iter_mut().find(|b| b.id == mill).unwrap().settings.paused = true;
        shed(&mut w, 10, 12, 1);
        run(&mut w, 5.0);
        assert!(w.units.iter().filter(|u| u.is_transport()).all(|u| u.job.is_none()), "nichts zu tun");
        // Nach einem kurzen Schritt läuft die Wartezeit noch: ohne Ereignis kein neuer Blick.
        w.step(0.05);
        w.inventories.get_mut(&mill).unwrap().add(Good::Boards, 2);
        w.dirty_buildings.insert(mill);
        w.step(0.05);
        assert!(w.units.iter().any(|u| u.is_transport() && u.job.is_some()), "Träger geht sofort los");
    }

    #[test]
    fn karren_fuellen_eingangslager_auf() {
        // Der Arbeiter trägt selbst (Standard), mit „Karren liefern“ bringt der Karren trotzdem Holz in großen Ladungen.
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let mill = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap().id;
        assert!(w.buildings.iter().find(|b| b.id == mill).unwrap().settings.cart_deliveries, "Standard: an");
        let s = &mut w.buildings.iter_mut().find(|b| b.id == mill).unwrap().settings;
        s.paused = false;
        s.cart_deliveries = true;
        shed(&mut w, 10, 12, 2);
        w.economy.def.army.civilian_respawn_seconds = 0.0; // entfernte Träger nicht ersetzen
        w.units.retain(|u| u.kind != UnitKind::Carrier);
        let cap = w.inventories[&mill].capacity_of(Good::Wood);
        let mut supplied = 0;
        for _ in 0..20 * 60 {
            w.step(0.05);
            if let Some(c) = w.units.iter().find(|u| u.kind == UnitKind::Cart) {
                if matches!(c.job, Some(super::Job { kind: super::JobKind::Supply { to }, .. }) if to == mill) {
                    supplied = supplied.max(c.carrying.map_or(0, |g| g.amount));
                }
            }
        }
        assert!(supplied > 2, "Karren bringt eine große Ladung ({supplied} von {cap})");
        // Ausgeschaltet: der Karren liefert nicht mehr.
        let mut s = w.buildings.iter().find(|b| b.id == mill).unwrap().settings.clone();
        s.cart_deliveries = false;
        w.configure(1, mill, s).unwrap();
        run(&mut w, 20.0);
        for _ in 0..20 * 40 {
            w.step(0.05);
            let c = w.units.iter().find(|u| u.kind == UnitKind::Cart).unwrap();
            assert!(!matches!(c.job, Some(super::Job { kind: super::JobKind::Supply { .. }, stage: super::Stage::Load(_), .. })), "keine neue Lieferung");
        }
    }

    #[test]
    fn neben_wagenschuppen_traegt_der_arbeiter_nicht_selbst() {
        let mut w = flat(80, 20);
        w.admin_give(1, &[GoodAmount { good: Good::Wood, amount: 200 }, GoodAmount { good: Good::Boards, amount: 200 }], 0);
        let far = w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        assert!(far.settings.worker_carries, "ohne Wagenschuppen trägt er selbst");
        w.economy.def.buildings.iter_mut().find(|d| d.kind == BuildingKind::CartShed).unwrap().levels.iter_mut().for_each(|l| l.service_radius = 15);
        w.place_building(1, BuildingKind::CartShed, 60, 2).unwrap();
        let near = w.place_building(1, BuildingKind::Sawmill, 66, 2).unwrap();
        assert!(!near.settings.worker_carries, "im Umkreis tragen die Träger");
        let out = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        assert!(out.settings.worker_carries, "außerhalb des Umkreises trägt er selbst");
    }

    #[test]
    fn ohne_lager_staut_es_sich_und_karren_faehrt_erst_ab_mindestmenge() {
        let mut w = flat(40, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        let orchard = w.place_building(1, BuildingKind::Orchard, 30, 2).unwrap();
        // Kein Lager: Obst bleibt liegen, bis das eigene Lager voll ist.
        let cap = w.economy.def.transport.building_capacity;
        run(&mut w, 25.0 * cap.div_ceil(2) as f32 + 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), cap);
        assert_eq!(
            w.buildings
                .iter()
                .find(|b| b.id == orchard.id)
                .unwrap()
                .status,
            BuildingStatus::NoStorageRoom
        );
        // Bergfried mit Trägern und ein Wagenschuppen: Die große Ladung lassen
        // die Träger für den Karren liegen, er holt alles auf einmal.
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        granary(&mut w, 2, 12);
        shed(&mut w, 10, 2, 2);
        let fruit = central(&w, Good::Fruit);
        let mut cart_load = 0;
        for _ in 0..20 * 40 {
            w.step(0.05);
            let cart = w.units.iter().find(|u| u.kind == UnitKind::Cart).unwrap();
            cart_load = cart_load.max(cart.carrying.map_or(0, |g| g.amount));
        }
        assert_eq!(cart_load, cap, "Karren holt alles auf einmal");
        assert!(central(&w, Good::Fruit) >= fruit + cap);
        // Ohne Träger: Kleinkram lässt auch der Karren liegen.
        w.economy.def.army.civilian_respawn_seconds = 0.0; // entfernte Träger nicht ersetzen
        w.units.retain(|u| u.kind != UnitKind::Carrier);
        // Wenig liegt herum: dafür fährt der Karren nicht. Erst anhalten und
        // eine laufende Fahrt zu Ende fahren lassen.
        let mut s = orchard.settings.clone();
        s.paused = true;
        w.configure(1, orchard.id, s).unwrap();
        run(&mut w, 40.0);
        let min = w.economy.def.transport.cart_min_load;
        set_fruit(&mut w, orchard.id, min - 1);
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), min - 1);
    }

    fn set_fruit(w: &mut World, id: u32, n: u32) {
        let inv = w.inventories.get_mut(&id).unwrap();
        let have = inv.amount(Good::Fruit);
        inv.take_all(&[GoodAmount { good: Good::Fruit, amount: have }]);
        inv.add(Good::Fruit, n);
    }

    fn pause(w: &mut World, id: u32) {
        let mut s = w.buildings.iter().find(|b| b.id == id).unwrap().settings.clone();
        s.paused = true;
        w.configure(1, id, s).unwrap();
    }

    /// Kein Brett mehr im Vorrat: Bretter gibt es nur noch im Sägewerk.
    fn ohne_bretter_im_vorrat(w: &mut World) {
        let n = central(w, Good::Boards);
        w.stocks.get_mut(&1).unwrap().take_all(&[GoodAmount { good: Good::Boards, amount: n }]);
    }

    fn sattlerei(w: &mut World, x: u32, y: u32) -> u32 {
        let id = w.place_building(1, BuildingKind::Tannery, x, y).unwrap().id;
        // Sättel erst ab höherer Stufe: für den Test direkt einstellen.
        w.buildings.iter_mut().find(|b| b.id == id).unwrap().settings.recipe = Some("make_saddle".into());
        id
    }

    #[test]
    fn traeger_bringen_ware_direkt_zum_abnehmer() {
        let mut w = flat(60, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }, GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        shed(&mut w, 30, 12, 1);
        let saw = w.place_building(1, BuildingKind::Sawmill, 40, 2).unwrap().id;
        let tan = sattlerei(&mut w, 50, 2);
        pause(&mut w, saw);
        ohne_bretter_im_vorrat(&mut w);
        w.inventories.get_mut(&saw).unwrap().add(Good::Boards, 2);
        let mut direct = false;
        for _ in 0..20 * 60 {
            w.step(0.05);
            direct |= w.units.iter().any(|u| matches!(u.job.map(|j| j.kind), Some(super::JobKind::Direct { from, to }) if from == saw && to == tan));
            assert_eq!(central(&w, Good::Boards), 0, "die Bretter gehen nicht übers Lager");
        }
        assert!(direct, "ein Träger bringt die Bretter direkt zur Sattlerei");
        assert_eq!(local(&w, tan, Good::Boards), 2);
        assert_eq!(local(&w, saw, Good::Boards), 0);
    }

    #[test]
    fn abgeholte_ware_geht_zum_abnehmer_statt_ins_lager() {
        let mut w = flat(60, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 100 }, GoodAmount { good: Good::Wood, amount: 100 }], 0);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        shed(&mut w, 30, 12, 1);
        let saw = w.place_building(1, BuildingKind::Sawmill, 40, 2).unwrap().id;
        let tan = sattlerei(&mut w, 50, 2);
        pause(&mut w, saw);
        ohne_bretter_im_vorrat(&mut w);
        w.inventories.get_mut(&saw).unwrap().add(Good::Boards, 2);
        // Ein Träger hat die Bretter schon zum Abholen (Richtung Lager) aufgeladen.
        let k = w.units.iter().position(|u| u.kind == UnitKind::Carrier).unwrap();
        let index: std::collections::HashMap<u32, usize> = w.buildings.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
        let door = w.door(w.buildings.iter().find(|b| b.id == saw).unwrap(), (0.0, 0.0)).unwrap();
        w.units[k].pos = door;
        let job = super::Job { kind: super::JobKind::Pickup { from: saw }, good: Good::Boards, amount: 2, stage: super::Stage::Load(saw), rest: super::NO_REST };
        w.arrive(k, job, &index);
        assert!(matches!(w.units[k].job.map(|j| (j.kind, j.stage)), Some((super::JobKind::Direct { .. }, super::Stage::Unload(t))) if t == tan));
        run(&mut w, 30.0);
        assert_eq!(local(&w, tan, Good::Boards), 2, "direkt in der Sattlerei angekommen");
        assert_eq!(central(&w, Good::Boards), 0);
    }

    #[test]
    fn lager_nimmt_nur_gewaehlte_waren() {
        let mut w = flat(40, 20);
        w.worker_carries_default = false; // hier tragen die Träger
        let keep = w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let mut s = keep.settings.clone();
        s.stores = Some(vec![Good::Wood]);
        w.configure(1, keep.id, s).unwrap();
        let orchard = w.place_building(1, BuildingKind::Orchard, 20, 2).unwrap();
        shed(&mut w, 10, 12, 1);
        pause(&mut w, orchard.id);
        set_fruit(&mut w, orchard.id, 4);
        run(&mut w, 30.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 4, "kein Lager nimmt Obst");
        granary(&mut w, 30, 12);
        run(&mut w, 40.0);
        assert_eq!(local(&w, orchard.id, Good::Fruit), 0, "jetzt in den Kornspeicher");
    }

    #[test]
    fn mindestbestand_bleibt_im_vorrat() {
        let mut w = flat(40, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 20, 2).unwrap();
        let wood = central(&w, Good::Wood);
        let mut s = saw.settings.clone();
        s.keep_in_stock = vec![GoodAmount { good: Good::Wood, amount: wood - 2 }];
        w.configure(1, saw.id, s).unwrap();
        // Ohne Wagenschuppen holt der Arbeiter des Sägewerks selbst.
        run(&mut w, 40.0);
        assert_eq!(central(&w, Good::Wood), wood - 2, "nur die 2 über dem Mindestbestand");
    }

    #[test]
    fn karren_sammelt_bei_mehreren_und_bleibt_im_umkreis() {
        let mut w = flat(120, 30);
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        granary(&mut w, 2, 12);
        shed(&mut w, 12, 2, 2);
        w.economy.def.army.civilian_respawn_seconds = 0.0; // entfernte Träger nicht ersetzen
        // Eine Plantage allein (5 Obst) reicht nicht für eine Fahrt, beide zusammen schon,
        // egal, welche Mindestladung das Balancing gerade hat.
        w.economy.def.transport.cart_min_load = 8;
        w.units.retain(|u| u.kind != UnitKind::Carrier);
        let a = w.place_building(1, BuildingKind::Orchard, 24, 2).unwrap();
        let b = w.place_building(1, BuildingKind::Orchard, 32, 2).unwrap();
        // Weit außerhalb des Umkreises (40 Felder): wird nie angefahren.
        let far = w.place_building(1, BuildingKind::Orchard, 100, 2).unwrap();
        for id in [a.id, b.id, far.id] {
            pause(&mut w, id);
        }
        set_fruit(&mut w, a.id, 5);
        set_fruit(&mut w, b.id, 5);
        set_fruit(&mut w, far.id, 12);
        let mut most = 0;
        for _ in 0..20 * 90 {
            w.step(0.05);
            let cart = w.units.iter().find(|u| u.kind == UnitKind::Cart).unwrap();
            most = most.max(cart.carrying.map_or(0, |g| g.amount));
        }
        assert_eq!(most, 10, "beide Plantagen auf einer Fahrt");
        assert_eq!(local(&w, far.id, Good::Fruit), 12);
    }

    #[test]
    fn abriss_gibt_inhalt_in_den_vorrat() {
        let mut w = flat(30, 20);
        w.place_building(1, BuildingKind::Keep, 2, 2).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 14, 2).unwrap();
        w.inventories.get_mut(&saw.id).unwrap().add(Good::Boards, 5);
        let boards = central(&w, Good::Boards);
        w.demolish(1, saw.id).unwrap();
        // 5 aus dem Lager plus Hälfte der Baukosten (keine Bretter beim Sägewerk).
        assert_eq!(central(&w, Good::Boards), boards + 5);
    }
}
