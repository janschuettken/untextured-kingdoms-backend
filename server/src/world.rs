//! Spielzustand: Einheiten und Gebäude auf der Karte.

use std::collections::{BTreeSet, HashMap};

use protocol::{
    BuildingKind, BuildingSettings, BuildingState, BuildingStatus, GoodAmount, ResourceKind, ServerMsg, StockState, Text,
    TileKind, UnitKind, UnitState,
};

use crate::{economy::Economy, map::GameMap, stock::Stock};

mod production;
mod transport;
use production::Work;
use transport::Job;

pub struct Unit {
    pub id: u32,
    pub owner: u32,
    pub kind: UnitKind,
    pub pos: (f32, f32),
    /// Noch abzulaufende Wegpunkte, der nächste zuerst.
    path: Vec<(f32, f32)>,
    /// Wohin die Einheit zuletzt geschickt wurde (für neue Wegsuche, wenn
    /// plötzlich ein Gebäude im Weg steht).
    target: Option<(f32, f32)>,
    /// Träger und Karren: das Gebäude, zu dem sie gehören.
    home: Option<u32>,
    /// Laufender Transportauftrag.
    job: Option<Job>,
    /// Ladung von Träger oder Karren.
    carrying: Option<GoodAmount>,
    /// Sekunden bis zur nächsten Auftragssuche.
    cooldown: f32,
}

impl Unit {
    fn new(id: u32, owner: u32, kind: UnitKind, pos: (f32, f32)) -> Unit {
        Unit { id, owner, kind, pos, path: Vec::new(), target: None, home: None, job: None, carrying: None, cooldown: 0.0 }
    }

    /// Träger und Karren gehorchen keinen Befehlen, sie arbeiten von selbst.
    fn is_transport(&self) -> bool {
        matches!(self.kind, UnitKind::Carrier | UnitKind::Cart)
    }
}

pub struct World {
    pub map: GameMap,
    pub units: Vec<Unit>,
    pub buildings: Vec<BuildingState>,
    /// Pro Feld die Id des Gebäudes darauf, 0 = frei.
    occupied: Vec<u32>,
    /// Felder vor Eingängen (Feld-Index → Gebäude-Id): dort darf nichts gebaut werden.
    entrances: HashMap<usize, u32>,
    next_unit_id: u32,
    next_building_id: u32,
    pub economy: Economy,
    /// Gemeinsamer Vorrat je Spieler (liegt im Bergfried und in den Vorratslagern).
    stocks: HashMap<u32, Stock>,
    /// Eigenes kleines Lager je Betrieb (Gebäude-Id): Eingänge und fertige Waren.
    inventories: HashMap<u32, Stock>,
    /// Gebäude, deren Status oder Lager sich geändert hat (gehen gesammelt raus).
    dirty_buildings: BTreeSet<u32>,
    /// Spieler, deren Lager sich seit dem letzten Senden geändert hat.
    dirty_stocks: BTreeSet<u32>,
    /// Wie viel jedes Feld noch hergibt (Wald, Stein, Eisen).
    resource_left: Vec<u32>,
    /// Laufende Arbeit je Gebäude-Id.
    work: HashMap<u32, Work>,
    /// Änderungen für alle Clients (Gebäudestatus, Rohstoffe), siehe `take_events`.
    events: Vec<ServerMsg>,
}

/// Feld vor dem Eingang einer Grundfläche (x, y, w, h), mittig an der Vorderseite.
/// Kann außerhalb der Karte liegen (dann negativ oder zu groß).
fn entrance_tile(x: u32, y: u32, w: u32, h: u32, rotation: u8) -> (i64, i64) {
    let (x, y, w, h) = (x as i64, y as i64, w as i64, h as i64);
    match rotation % 4 {
        0 => (x + w / 2, y + h),
        1 => (x - 1, y + h / 2),
        2 => (x + (w - 1) / 2, y - 1),
        _ => (x + w, y + (h - 1) / 2),
    }
}

/// Fehlertext für den Client (Schlüssel, übersetzt wird dort).
type Refusal = Text;

/// Name eines Rohstoffs wie im Protokoll (`forest`, ...), für Text-Platzhalter.
fn resource_key(r: ResourceKind) -> String {
    serde_json::to_value(r).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

impl World {
    pub fn new(map: GameMap, economy: Economy) -> World {
        let n = (map.width * map.height) as usize;
        let resource_left = map.resources.iter().map(|&r| economy.resource_amount(r)).collect();
        World {
            map,
            units: Vec::new(),
            buildings: Vec::new(),
            occupied: vec![0; n],
            entrances: HashMap::new(),
            next_unit_id: 1,
            next_building_id: 1,
            economy,
            stocks: HashMap::new(),
            inventories: HashMap::new(),
            dirty_buildings: BTreeSet::new(),
            dirty_stocks: BTreeSet::new(),
            resource_left,
            work: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Änderungen seit dem letzten Aufruf, zum Verteilen an alle Clients.
    pub fn take_events(&mut self) -> Vec<ServerMsg> {
        std::mem::take(&mut self.events)
    }

    pub fn stock_state(&self, player: u32) -> StockState {
        self.stocks.get(&player).map(|s| s.state()).unwrap_or(StockState { goods: Vec::new(), capacity: 0 })
    }

    /// Lager, die sich geändert haben, seit dem letzten Aufruf.
    pub fn take_dirty_stocks(&mut self) -> Vec<(u32, StockState)> {
        let dirty = std::mem::take(&mut self.dirty_stocks);
        dirty.into_iter().filter(|p| self.stocks.contains_key(p)).map(|p| (p, self.stock_state(p))).collect()
    }

    /// Kapazität je Ware: Grundwert plus je Vorratslager.
    fn update_capacity(&mut self, player: u32) {
        let n = self.buildings.iter().filter(|b| b.owner == player && b.kind == BuildingKind::Storehouse).count();
        let cap = self.economy.def.base_capacity + n as u32 * self.economy.def.storehouse_capacity;
        if let Some(s) = self.stocks.get_mut(&player) {
            s.capacity = cap;
            self.dirty_stocks.insert(player);
        }
    }

    /// Neue Wirtschaftsdaten aus dem Balancing-Editor übernehmen. Stehende
    /// Gebäude bleiben; Lagergrößen passen sich an, Rezepte, die es nicht mehr
    /// gibt, laufen nicht weiter.
    pub fn set_economy(&mut self, economy: Economy) {
        self.economy = economy;
        let cap = self.economy.def.transport.building_capacity;
        for inv in self.inventories.values_mut() {
            inv.capacity = cap;
        }
        let players: Vec<u32> = self.stocks.keys().copied().collect();
        for p in players {
            self.update_capacity(p);
        }
        for b in &self.buildings {
            self.dirty_buildings.insert(b.id);
        }
    }

    /// Admin: Waren direkt ins Lager legen (höchstens bis zur Kapazität).
    pub fn admin_give(&mut self, player: u32, goods: &[GoodAmount]) {
        let Some(stock) = self.stocks.get_mut(&player) else { return };
        for g in goods {
            if self.economy.def.goods.contains(&g.good) {
                stock.add(g.good, g.amount);
            }
        }
        self.dirty_stocks.insert(player);
    }

    /// Neuer Spieler: bekommt einen König auf einem freien Startfeld.
    pub fn spawn_player(&mut self, player: u32) {
        let taken: Vec<(u32, u32)> = self
            .units
            .iter()
            .filter(|u| u.kind == UnitKind::King)
            .map(|u| (u.pos.0 as u32, u.pos.1 as u32))
            .collect();
        let spawns = &self.map.spawns;
        let (x, y) = spawns
            .iter()
            .copied()
            .find(|s| !taken.contains(s))
            .unwrap_or(spawns[player as usize % spawns.len()]);
        let id = self.next_unit_id;
        self.next_unit_id += 1;
        let mut stock = Stock::new(self.economy.def.base_capacity);
        for g in &self.economy.def.start_stock {
            stock.add(g.good, g.amount);
        }
        self.stocks.insert(player, stock);
        self.units.push(Unit::new(id, player, UnitKind::King, (x as f32 + 0.5, y as f32 + 0.5)));
    }

    /// Spieler geht: seine Einheiten und Gebäude verschwinden.
    /// Liefert die Ids der entfernten Gebäude.
    pub fn remove_player(&mut self, player: u32) -> Vec<u32> {
        self.units.retain(|u| u.owner != player);
        let ids: Vec<u32> = self.buildings.iter().filter(|b| b.owner == player).map(|b| b.id).collect();
        for &id in &ids {
            self.remove_building(id);
        }
        self.stocks.remove(&player);
        ids
    }

    /// Gebäude errichten, wenn der Platz passt und das Lager reicht. Ein
    /// Gebäude braucht flachen Boden auf einer einzigen Höhenstufe, keine
    /// Rampe, kein Wasser, keinen Wald oder Fels, und darf weder ein anderes
    /// Gebäude noch eine Einheit überdecken.
    #[cfg(test)]
    pub fn place_building(&mut self, player: u32, kind: BuildingKind, x: u32, y: u32) -> Result<BuildingState, Refusal> {
        self.place_building_rotated(player, kind, x, y, 0)
    }

    /// Wie `place_building`, gedreht um `rotation` Vierteldrehungen (siehe Protokoll).
    /// Vor dem Eingang muss ein begehbares Feld frei sein, und kein Gebäude darf
    /// den Eingang eines anderen zubauen.
    pub fn place_building_rotated(
        &mut self,
        player: u32,
        kind: BuildingKind,
        x: u32,
        y: u32,
        rotation: u8,
    ) -> Result<BuildingState, Refusal> {
        let def = self.economy.building(kind).ok_or_else(|| Text::new("error.unknown_building"))?.clone();
        let rotation = rotation % 4;
        let (w, h) = if rotation % 2 == 1 { (def.h, def.w) } else { (def.w, def.h) };
        let max_hp = def.max_hp;
        let cost = def.levels[0].cost.clone();
        if let Some(max) = def.max_per_player {
            let have = self.buildings.iter().filter(|b| b.owner == player && b.kind == kind).count();
            if have >= max as usize {
                return Err(Text::new("error.limit_reached").arg("max", max));
            }
        }
        if x.checked_add(w).is_none_or(|r| r > self.map.width) || y.checked_add(h).is_none_or(|b| b > self.map.height) {
            return Err(Text::new("error.outside_map"));
        }
        let level = self.map.levels[(y * self.map.width + x) as usize];
        for ty in y..y + h {
            for tx in x..x + w {
                let i = (ty * self.map.width + tx) as usize;
                match self.map.tiles[i] {
                    TileKind::Water => return Err(Text::new("error.on_water")),
                    TileKind::Ramp => return Err(Text::new("error.on_ramp")),
                    TileKind::Ground => {}
                }
                if self.map.levels[i] != level {
                    return Err(Text::new("error.not_flat"));
                }
                if self.occupied[i] != 0 {
                    return Err(Text::new("error.occupied"));
                }
                if self.entrances.contains_key(&i) {
                    return Err(Text::new("error.blocks_entrance"));
                }
                let r = self.map.resources[i];
                let allowed = def.placement.as_ref().is_some_and(|p| p.resources.contains(&r));
                if crate::map::resource_blocks(r) && !allowed {
                    return Err(Text::new("error.on_resource"));
                }
            }
        }
        if let Some(p) = &def.placement {
            let on = (y..y + h)
                .flat_map(|ty| (x..x + w).map(move |tx| (tx, ty)))
                .filter(|&(tx, ty)| p.resources.contains(&self.map.resources[(ty * self.map.width + tx) as usize]))
                .count() as u32;
            if on * 100 < p.min_share_percent * w * h {
                let names: Vec<String> = p.resources.iter().map(|r| resource_key(*r)).collect();
                return Err(Text::new("error.needs_resource").arg("resources", names.join(",")));
            }
        }
        let entrance = if def.no_entrance { None } else { Some(entrance_tile(x, y, w, h, rotation)) };
        if let Some((ex, ey)) = entrance {
            let inside = ex >= 0 && ey >= 0 && (ex as u32) < self.map.width && (ey as u32) < self.map.height;
            if !inside || !self.map.nav.is_open(ex as i32, ey as i32) {
                return Err(Text::new("error.entrance_blocked"));
            }
        }
        let entrance = entrance.map(|(ex, ey)| (ex as u32, ey as u32));
        let covers = |p: (f32, f32)| {
            let (ux, uy) = (p.0.floor() as i64, p.1.floor() as i64);
            ux >= x as i64 && uy >= y as i64 && ux < (x + w) as i64 && uy < (y + h) as i64
        };
        // Träger weichen aus, alle anderen Einheiten stehen im Weg.
        if self.units.iter().any(|u| !u.is_transport() && covers(u.pos)) {
            return Err(Text::new("error.unit_in_way"));
        }
        self.pay(player, &cost)?;

        let settings = BuildingSettings {
            recipe: def.levels[0].recipes.first().cloned(),
            paused: false,
            accept_deliveries: true,
            stores: None,
            keep_in_stock: Vec::new(),
        };
        let status = if settings.recipe.is_some() { BuildingStatus::Working } else { BuildingStatus::Idle };
        let b = BuildingState {
            id: self.next_building_id,
            owner: player,
            kind,
            x,
            y,
            w,
            h,
            hp: max_hp,
            level: 1,
            settings,
            status,
            rotation,
            entrance,
            inventory: Vec::new(),
        };
        self.next_building_id += 1;
        if let Some((ex, ey)) = entrance {
            self.entrances.insert((ey * self.map.width + ex) as usize, b.id);
        }
        for ty in y..y + h {
            for tx in x..x + w {
                self.occupied[(ty * self.map.width + tx) as usize] = b.id;
                self.map.nav.set_blocked(tx as i32, ty as i32, true);
            }
        }
        self.buildings.push(b.clone());
        if !def.hub {
            self.inventories.insert(b.id, Stock::new(self.economy.def.transport.building_capacity));
        }
        if kind == BuildingKind::Storehouse {
            self.update_capacity(player);
        }
        self.make_room(&b);
        for s in &def.spawns {
            for n in 0..s.count {
                self.spawn_transport(&b, s.kind, n);
            }
        }
        self.replan_all();
        Ok(b)
    }

    /// Kosten aus dem Lager nehmen oder mit Grund ablehnen.
    fn pay(&mut self, player: u32, cost: &[GoodAmount]) -> Result<(), Refusal> {
        let stock = self.stocks.get_mut(&player).ok_or_else(|| Text::new("error.no_player"))?;
        if let Some(missing) = cost.iter().find(|c| stock.amount(c.good) < c.amount) {
            let good = serde_json::to_value(missing.good).ok().and_then(|v| v.as_str().map(String::from));
            return Err(Text::new("error.not_enough")
                .arg("good", good.unwrap_or_default())
                .arg("need", missing.amount - stock.amount(missing.good)));
        }
        stock.take_all(cost);
        self.dirty_stocks.insert(player);
        Ok(())
    }

    /// Eigenes Gebäude auf die nächste Stufe bringen.
    pub fn upgrade(&mut self, player: u32, id: u32) -> Result<BuildingState, Refusal> {
        let b = self.buildings.iter().find(|b| b.id == id).ok_or_else(|| Text::new("error.no_building"))?;
        if b.owner != player {
            return Err(Text::new("error.not_yours"));
        }
        let def = self.economy.building(b.kind).ok_or_else(|| Text::new("error.unknown_building"))?;
        let Some(next) = def.levels.get(b.level as usize) else {
            return Err(Text::new("error.max_level"));
        };
        let cost = next.cost.clone();
        self.pay(player, &cost)?;
        let b = self.buildings.iter_mut().find(|b| b.id == id).expect("eben gefunden");
        b.level += 1;
        Ok(b.clone())
    }

    /// Eigenes Gebäude abreißen. Ein Teil aller Bau- und Ausbaukosten kommt zurück.
    pub fn demolish(&mut self, player: u32, id: u32) -> Result<(), Refusal> {
        let b = match self.buildings.iter().find(|b| b.id == id) {
            Some(b) if b.owner == player => b.clone(),
            Some(_) => return Err(Text::new("error.not_yours")),
            None => return Err(Text::new("error.no_building")),
        };
        let pct = self.economy.def.demolish_refund_percent;
        let mut refund: Vec<GoodAmount> = Vec::new();
        if let Some(def) = self.economy.building(b.kind) {
            for level in def.levels.iter().take(b.level as usize) {
                refund.extend(level.cost.iter().map(|c| GoodAmount { good: c.good, amount: c.amount * pct / 100 }));
            }
        }
        self.remove_building(id);
        if b.kind == BuildingKind::Storehouse {
            self.update_capacity(player);
        }
        if let Some(stock) = self.stocks.get_mut(&player) {
            for g in refund {
                stock.add(g.good, g.amount);
            }
            self.dirty_stocks.insert(player);
        }
        self.replan_all();
        Ok(())
    }

    /// Gebäude entfernen. Was in ihm lag und was seine Träger tragen, kommt in den Vorrat.
    fn remove_building(&mut self, id: u32) {
        let Some(pos) = self.buildings.iter().position(|b| b.id == id) else { return };
        let b = self.buildings.remove(pos);
        self.work.remove(&id);
        self.dirty_buildings.remove(&id);
        self.entrances.retain(|_, b| *b != id);
        let mut back: Vec<GoodAmount> = self.inventories.remove(&id).map(|s| s.state().goods).unwrap_or_default();
        back.extend(self.units.iter().filter(|u| u.home == Some(id)).filter_map(|u| u.carrying));
        self.units.retain(|u| u.home != Some(id));
        if let Some(stock) = self.stocks.get_mut(&b.owner) {
            for g in back {
                stock.add(g.good, g.amount);
            }
            self.dirty_stocks.insert(b.owner);
        }
        for ty in b.y..b.y + b.h {
            for tx in b.x..b.x + b.w {
                self.occupied[(ty * self.map.width + tx) as usize] = 0;
                self.map.nav.set_blocked(tx as i32, ty as i32, false);
            }
        }
    }

    /// Nach einer Änderung am Gitter: laufende Einheiten suchen ihren Weg neu.
    fn replan_all(&mut self) {
        for unit in self.units.iter_mut() {
            if let (Some(target), false) = (unit.target, unit.path.is_empty()) {
                unit.path = self.map.nav.waypoints(unit.pos, target);
            }
        }
    }

    /// Bewegungsbefehl eines Spielers. Fremde Einheiten werden ignoriert.
    pub fn order_move(&mut self, player: u32, unit_ids: &[u32], target: (f32, f32)) {
        if !target.0.is_finite() || !target.1.is_finite() {
            return;
        }
        let target = (
            target.0.clamp(0.0, self.map.width as f32 - 0.01),
            target.1.clamp(0.0, self.map.height as f32 - 0.01),
        );
        for unit in self.units.iter_mut() {
            if unit.owner == player && !unit.is_transport() && unit_ids.contains(&unit.id) {
                unit.path = self.map.nav.waypoints(unit.pos, target);
                unit.target = Some(target);
            }
        }
    }

    /// Einen Spieltakt von `dt` Sekunden simulieren.
    pub fn step(&mut self, dt: f32) {
        self.step_production(dt);
        self.step_transport(dt);
        let t = &self.economy.def.transport;
        let (carrier, cart) = (t.carrier_speed, t.cart_speed);
        let king = self.economy.def.movement.king_speed;
        for unit in self.units.iter_mut() {
            let speed = match unit.kind {
                UnitKind::King => king,
                UnitKind::Carrier => carrier,
                UnitKind::Cart => cart,
            };
            let mut budget = speed * dt;
            while budget > 0.0 {
                let Some(&next) = unit.path.first() else { break };
                let (dx, dy) = (next.0 - unit.pos.0, next.1 - unit.pos.1);
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= budget {
                    unit.pos = next;
                    unit.path.remove(0);
                    budget -= dist;
                } else {
                    unit.pos.0 += dx / dist * budget;
                    unit.pos.1 += dy / dist * budget;
                    budget = 0.0;
                }
            }
        }
        self.flush_buildings();
    }

    /// Geänderte Gebäude (Status, Lager) an alle Clients melden.
    fn flush_buildings(&mut self) {
        for id in std::mem::take(&mut self.dirty_buildings) {
            let Some(b) = self.buildings.iter_mut().find(|b| b.id == id) else { continue };
            b.inventory = self.inventories.get(&id).map(|s| s.state().goods).unwrap_or_default();
            self.events.push(ServerMsg::BuildingChanged { building: b.clone() });
        }
    }

    pub fn snapshot(&self) -> Vec<UnitState> {
        self.units
            .iter()
            .map(|u| UnitState {
                id: u.id,
                owner: u.owner,
                kind: u.kind,
                x: u.pos.0,
                y: u.pos.1,
                moving: !u.path.is_empty(),
                carrying: u.carrying.map(|g| g.good),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::DEFAULT_MAP;

    #[test]
    fn koenig_laeuft_aus_der_burg_ins_tal() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap(), eco());
        world.spawn_player(1);
        world.spawn_player(2);
        let id = world.units[0].id;
        world.order_move(1, &[id], (122.5, 82.5));
        world.order_move(1, &[world.units[1].id], (0.5, 0.5)); // fremde Einheit: ignoriert
        assert!(world.units[1].path.is_empty());
        for _ in 0..20 * 120 {
            world.step(0.05);
            let u = &world.units[0];
            let (x, y) = (u.pos.0.floor() as i32, u.pos.1.floor() as i32);
            assert!(world.map.nav.is_open(x, y), "König steht auf ({x}, {y}) im Wasser/Fels");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 122.5).abs() < 1e-3 && (u.pos.1 - 82.5).abs() < 1e-3, "König bei {:?}", u.pos);
    }

    fn eco() -> Economy {
        Economy::parse(crate::economy::ECONOMY_JSON).unwrap()
    }

    fn flat_world() -> World {
        let row = "0".repeat(24);
        let text = format!("spawn 1 1\n{}", format!("{row}\n").repeat(24));
        let mut world = World::new(GameMap::parse(&text).unwrap(), eco());
        world.spawn_player(1);
        world
    }

    #[test]
    fn bauregeln_werden_geprueft() {
        let mut world = World::new(GameMap::parse(DEFAULT_MAP).unwrap(), eco());
        world.spawn_player(1);
        let w = world.map.width;
        let water = (0..w * world.map.height).find(|&i| world.map.tiles[i as usize] == TileKind::Water).unwrap();
        assert!(world.place_building(1, BuildingKind::Wall, water % w, water / w).is_err());
        let ramp = (0..w * world.map.height).find(|&i| world.map.tiles[i as usize] == TileKind::Ramp).unwrap();
        assert!(world.place_building(1, BuildingKind::Wall, ramp % w, ramp / w).is_err());
        assert!(world.place_building(1, BuildingKind::Keep, w - 2, w - 2).is_err(), "ragt aus der Karte");
        let king = world.units[0].pos;
        assert!(world.place_building(1, BuildingKind::Wall, king.0 as u32, king.1 as u32).is_err());

        let mut flat = flat_world();
        let b = flat.place_building(1, BuildingKind::Tower, 4, 4).unwrap();
        assert_eq!((b.w, b.h), (3, 3));
        assert!(flat.place_building(1, BuildingKind::Wall, 5, 5).is_err(), "überlappt");
        assert!(!flat.map.nav.is_open(5, 5));
        flat.place_building(1, BuildingKind::Keep, 10, 10).unwrap();
        assert!(flat.place_building(1, BuildingKind::Keep, 0, 12).is_err(), "nur ein Bergfried");
        assert!(flat.demolish(2, b.id).is_err(), "fremdes Gebäude");
        flat.demolish(1, b.id).unwrap();
        assert!(flat.map.nav.is_open(5, 5));
    }

    #[test]
    fn bauen_kostet_und_abriss_erstattet() {
        use protocol::Good;
        let mut w = flat_world();
        let wood = w.stocks[&1].amount(Good::Wood);
        let b = w.place_building(1, BuildingKind::Bowyer, 4, 4).unwrap();
        let s = &w.stocks[&1];
        assert_eq!(s.amount(Good::Wood), wood - 20);
        assert_eq!(b.level, 1);
        // Stufe 2 braucht Eisen, das es zu Beginn nicht gibt.
        let err = w.upgrade(1, b.id).unwrap_err();
        assert_eq!(err.key, "error.not_enough");
        assert_eq!(err.args["good"], "iron");
        w.stocks.get_mut(&1).unwrap().add(Good::Iron, 10);
        assert_eq!(w.upgrade(1, b.id).unwrap().level, 2);
        assert_eq!(w.upgrade(1, b.id).unwrap_err().key, "error.max_level");
        assert_eq!(w.upgrade(2, b.id).unwrap_err().key, "error.not_yours");
        let boards = w.stocks[&1].amount(Good::Boards);
        w.demolish(1, b.id).unwrap();
        // Hälfte von Stufe 1 (20 Bretter) und Stufe 2 (30 Bretter) zurück.
        assert_eq!(w.stocks[&1].amount(Good::Boards), boards + 10 + 15);
        assert!(!w.take_dirty_stocks().is_empty());
        assert!(w.take_dirty_stocks().is_empty(), "nur einmal melden");
    }

    #[test]
    fn ohne_material_kein_bau_und_lager_erhoeht_platz() {
        use protocol::Good;
        let mut w = flat_world();
        let cap = w.stocks[&1].capacity;
        w.place_building(1, BuildingKind::Storehouse, 6, 6).unwrap();
        assert_eq!(w.stocks[&1].capacity, cap + w.economy.def.storehouse_capacity);
        let all = w.stocks[&1].amount(Good::Stone);
        w.stocks.get_mut(&1).unwrap().take_all(&[GoodAmount { good: Good::Stone, amount: all }]);
        assert_eq!(w.place_building(1, BuildingKind::Wall, 0, 9).unwrap_err().key, "error.not_enough");
    }

    #[test]
    fn drehen_und_eingang_freihalten() {
        let mut w = flat_world();
        // Sägewerk 4x4, Eingang unten mittig.
        let b = w.place_building(1, BuildingKind::Sawmill, 4, 4).unwrap();
        assert_eq!(b.entrance, Some((6, 8)));
        assert_eq!(w.place_building(1, BuildingKind::Wall, 6, 8).unwrap_err().key, "error.blocks_entrance");
        w.place_building(1, BuildingKind::Wall, 5, 8).unwrap();
        // Eingang an den Kartenrand: geht nicht. Gedreht (Eingang links) schon.
        assert_eq!(w.place_building(1, BuildingKind::Sawmill, 12, 20).unwrap_err().key, "error.entrance_blocked");
        let r = w.place_building_rotated(1, BuildingKind::Sawmill, 12, 20, 1).unwrap();
        assert_eq!((r.rotation, r.entrance), (1, Some((11, 22))));
        // Tower ohne Eingang darf direkt anschließen.
        let t = w.place_building(1, BuildingKind::Tower, 16, 20).unwrap();
        assert_eq!(t.entrance, None);
        // Formel für andere Seiten.
        assert_eq!(entrance_tile(0, 0, 4, 2, 3), (4, 0));
        assert_eq!(entrance_tile(5, 5, 3, 3, 2), (6, 4));
        w.demolish(1, b.id).unwrap();
        w.place_building(1, BuildingKind::Wall, 6, 8).unwrap();
    }

    #[test]
    fn nicht_auf_wald_bauen() {
        let text = "spawn 0 0\n[terrain]\n000\n000\n[resources]\n..W\n...\n";
        let mut w = World::new(GameMap::parse(text).unwrap(), eco());
        w.spawn_player(1);
        assert_eq!(w.place_building(1, BuildingKind::Wall, 2, 0).unwrap_err().key, "error.on_resource");
        assert!(w.place_building(1, BuildingKind::Wall, 2, 1).is_ok());
    }

    #[test]
    fn mauer_im_weg_wird_umgangen() {
        let mut world = flat_world();
        let id = world.units[0].id;
        world.order_move(1, &[id], (10.5, 1.5));
        world.step(0.05);
        // Quer über den Weg eine Mauer mit Lücke ganz unten.
        for y in 0..11 {
            world.place_building(1, BuildingKind::Wall, 5, y).unwrap();
        }
        for _ in 0..20 * 30 {
            world.step(0.05);
            let u = &world.units[0];
            assert!(world.map.nav.is_open(u.pos.0 as i32, u.pos.1 as i32), "König läuft durch die Mauer");
        }
        let u = &world.units[0];
        assert!((u.pos.0 - 10.5).abs() < 1e-3 && (u.pos.1 - 1.5).abs() < 1e-3);
    }
}
