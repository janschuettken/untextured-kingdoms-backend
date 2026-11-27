//! Weitere Überlegungen des Computergegners: Holzfäller versetzen, Brücken schlagen,
//! Wagenkarren nutzen, nach fertiger Burg weiterbauen und Angriffe mit Verbündeten abstimmen.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use protocol::{BuildingCategory, BuildingKind, ClientMsg, Good, ResourceKind, StockState};
use tracing::debug;

use super::{layout, Computer, Layout};
use crate::world::{level_cost, World};

/// So oft prüft er, ob ein Holzfäller noch Wald in der Nähe hat.
const RELOCATE_SECONDS: f32 = 30.0;
/// So weit (Felder vom Gebäuderand) muss noch Wald stehen, sonst zieht der Holzfäller um.
const FOREST_NEAR: i64 = 12;
/// Weniger Waldfelder als das in der Nähe: abgeholzt.
const FOREST_MIN: usize = 4;
/// Am neuen Platz muss mindestens so viel Wald in der Nähe stehen.
const FOREST_NEW: usize = 12;
/// So weit sucht er nach dem nächsten Wald.
const FOREST_SEARCH: i64 = 90;
/// So oft prüft er (mit bereiter Welle), ob eine Brücke den Weg zum Feind öffnet oder abkürzt.
const BRIDGE_SECONDS: f32 = 90.0;
/// Höchstens so viele Brücken baut er.
const BRIDGE_MAX: u32 = 4;
/// Eine Brücke lohnt, wenn der Weg damit höchstens diesen Anteil des Umwegs lang ist.
const BRIDGE_GAIN: f32 = 0.65;
/// Höchstens so viele Wagenschuppen (mehr verteilen die Karren nur dünner).
const CART_SHEDS_MAX: u32 = 3;
/// Ab dieser Stufe ist ein Wagenschuppen stark genug (Karren), dass die Arbeiter in seinem
/// Umkreis nicht mehr selbst tragen.
const CART_LEVEL: u32 = 2;
/// So viele Betriebe übernimmt ein Karren (seine Arbeiter tragen dann nicht mehr selbst).
const PER_CART: u32 = 3;
/// Meldungen der Verbündeten, die älter sind (Spielsekunden), gelten nicht mehr.
const CALL_FRESH: f64 = 45.0;
/// Weiterbauen: so viele Runden höchstens (bei vollem Regler).
const GROW_ROUNDS_MAX: f32 = 24.0;

/// Was ein Computergegner seinen Verbündeten über seine Angriffe mitteilt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AiCall {
    /// Gegen wen.
    pub target: u32,
    /// Stärke seiner Welle.
    pub power: f32,
    /// Unterwegs (sonst: steht bereit, ist aber allein zu schwach).
    pub marching: bool,
    /// Spielzeit der Meldung.
    pub at: f64,
}

/// Wasserfelder einer Brücke: oben links, Breite, Höhe.
type Span = (u32, u32, u32, u32);
/// Eine Stelle für eine Brücke: beide Ufer und die Wasserfelder.
type Crossing = ((i64, i64), (i64, i64), Span);

/// Gemeinsame Tafel aller Computergegner eines Spiels (Absprachen unter Verbündeten).
pub type Board = Arc<Mutex<HashMap<u32, AiCall>>>;

/// Reihenfolge, in der er nach fertiger Burg weiter ausbaut.
const GROWTH: [BuildingKind; 12] = [
    BuildingKind::Woodcutter,
    BuildingKind::House,
    BuildingKind::Orchard,
    BuildingKind::Quarry,
    BuildingKind::Sawmill,
    BuildingKind::House,
    BuildingKind::Woodcutter,
    BuildingKind::Stonemason,
    BuildingKind::Granary,
    BuildingKind::Orchard,
    BuildingKind::Storehouse,
    BuildingKind::Barracks,
];

/// Was nach den Befehlen dieser Überlegung noch im Lager liegt (sonst lehnt der Server den
/// zweiten Befehl ab, der dieselben Waren braucht).
pub(super) fn available(world: &World, stock: &StockState, cmds: &[ClientMsg]) -> HashMap<Good, u32> {
    let mut left: HashMap<Good, u32> = stock.goods.iter().map(|g| (g.good, g.amount)).collect();
    let building = |id: u32| world.buildings.iter().find(|b| b.id == id);
    for cmd in cmds {
        let cost: Vec<protocol::GoodAmount> = match cmd {
            ClientMsg::PlaceBuilding { kind, .. } => world.economy.building(*kind).map(|d| level_cost(d, 1)).unwrap_or_default(),
            ClientMsg::UpgradeBuilding { building_id } => building(*building_id)
                .and_then(|b| world.economy.building(b.kind).and_then(|d| d.levels.get(b.level as usize)).map(|l| l.cost.clone()))
                .unwrap_or_default(),
            ClientMsg::MoveBuilding { building_id, .. } => building(*building_id).map(|b| world.move_cost(b)).unwrap_or_default(),
            ClientMsg::PlaceBridge { w, h, .. } => world.bridge_cost((*w).max(*h)),
            ClientMsg::RepairBuilding { building_id } => building(*building_id).map(|b| world.repair_cost(b)).unwrap_or_default(),
            _ => Vec::new(),
        };
        for c in cost {
            let l = left.entry(c.good).or_default();
            *l = l.saturating_sub(c.amount);
        }
    }
    left
}

fn center(b: &protocol::BuildingState) -> (f32, f32) {
    (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0)
}

/// Waldfelder höchstens `r` Felder vom Rechteck entfernt.
fn forest_near(world: &World, x0: i64, y0: i64, x1: i64, y1: i64, r: i64) -> usize {
    let (w, h) = (world.map.width as i64, world.map.height as i64);
    let mut n = 0;
    for y in (y0 - r).max(0)..=(y1 + r).min(h - 1) {
        for x in (x0 - r).max(0)..=(x1 + r).min(w - 1) {
            if world.map.resources[(y * w + x) as usize] == ResourceKind::Forest {
                n += 1;
            }
        }
    }
    n
}

impl Computer {
    /// Gemeinsame Tafel setzen (alle Computergegner eines Spiels teilen eine).
    pub fn set_board(&mut self, board: Board) {
        self.board = board;
    }

    /// Wie viele Soldaten er gerade will: Profil, dazu Zuwachs nach fertiger Burg und nach
    /// abgebrochenen Angriffen.
    pub(super) fn army_target(&self) -> u32 {
        self.profile.army_size + self.grow_army + self.fail_army
    }

    /// Holzfäller, um die der Wald abgeholzt ist, ziehen zum nächsten Wald (einer je Prüfung).
    pub(super) fn relocate_woodcutters(&mut self, world: &World, layout: &Layout, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        if self.clock < self.next_relocate {
            return;
        }
        self.next_relocate = self.clock + RELOCATE_SECONDS;
        let p = self.player;
        let (w, h) = (world.map.width as i64, world.map.height as i64);
        for b in world.buildings.iter().filter(|b| b.owner == p && b.kind == BuildingKind::Woodcutter) {
            let (x0, y0, x1, y1) = (b.x as i64, b.y as i64, (b.x + b.w) as i64 - 1, (b.y + b.h) as i64 - 1);
            if forest_near(world, x0, y0, x1, y1, FOREST_NEAR) >= FOREST_MIN {
                continue;
            }
            let left = available(world, stock, cmds);
            if !world.move_cost(b).iter().all(|c| left.get(&c.good).copied().unwrap_or(0) >= c.amount) {
                return;
            }
            // Nächster Wald (Ringe um das Gebäude), der noch einigermaßen dicht ist.
            let c = center(b);
            let (cx, cy) = (c.0 as i64, c.1 as i64);
            let mut target = None;
            'search: for r in FOREST_NEAR..=FOREST_SEARCH {
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx.abs().max(dy.abs()) != r {
                            continue;
                        }
                        let (x, y) = (cx + dx, cy + dy);
                        if x < 0 || y < 0 || x >= w || y >= h || world.map.resources[(y * w + x) as usize] != ResourceKind::Forest {
                            continue;
                        }
                        if forest_near(world, x, y, x, y, 4) >= FOREST_NEW {
                            target = Some((x, y));
                            break 'search;
                        }
                    }
                }
            }
            let Some(near) = target else { continue };
            let Some((x, y, rotation)) = layout::find_site(world, p, BuildingKind::Woodcutter, layout, Some(near)) else { continue };
            let (bw, bh) = if rotation % 2 == 1 { (b.h as i64, b.w as i64) } else { (b.w as i64, b.h as i64) };
            if forest_near(world, x as i64, y as i64, x as i64 + bw - 1, y as i64 + bh - 1, FOREST_NEAR) < FOREST_NEW {
                continue;
            }
            debug!("Computer {p}: Holzfäller {} zieht um nach {x},{y}", b.id);
            cmds.push(ClientMsg::MoveBuilding { building_id: b.id, x, y, rotation });
            return;
        }
    }

    /// Brücke, wenn der Feind sonst nicht erreichbar ist oder der Weg zu ihm ein großer Umweg
    /// ist (etwa um einen Fluss oder um eine Mauer herum, die an einem Fluss endet).
    /// `from`: wo die Welle steht; `door`: Eingang des feindlichen Bergfrieds.
    pub(super) fn plan_bridge(&mut self, world: &World, stock: &StockState, cmds: &[ClientMsg], from: (f32, f32), door: (u32, u32)) -> Option<ClientMsg> {
        if self.clock < self.next_bridge || self.bridges >= BRIDGE_MAX {
            return None;
        }
        self.next_bridge = self.clock + BRIDGE_SECONDS;
        let nav = &world.map.nav;
        let start = (from.0 as i32, from.1 as i32);
        let goal = (door.0 as i32, door.1 as i32);
        let dist = |a: (i32, i32), b: (i32, i32)| ((a.0 - b.0) as f32).hypot((a.1 - b.1) as f32);
        let path = nav.find_path(start, goal);
        let end = path.last().copied().unwrap_or(start);
        let reachable = dist(end, goal) <= 6.0;
        let walk = path.len() as f32;
        let straight = dist(start, goal);
        if reachable && walk < straight * 1.6 + 20.0 {
            return None;
        }
        let span = world.max_bridge_span() as i64;
        let map = &world.map;
        // Wasserfelder auf der Luftlinie und rund um das Wegende (bis dorthin kommt man).
        let mut probes: Vec<(i64, i64)> = Vec::new();
        let steps = straight.ceil() as i32;
        for k in 0..=steps {
            let t = k as f32 / steps.max(1) as f32;
            let q = ((start.0 as f32 + (goal.0 - start.0) as f32 * t) as i64, (start.1 as f32 + (goal.1 - start.1) as f32 * t) as i64);
            if !map.is_land(q.0, q.1) && !probes.contains(&q) {
                probes.push(q);
            }
        }
        if !reachable {
            for dy in -8..=8i64 {
                for dx in -8..=8i64 {
                    let q = (end.0 as i64 + dx, end.1 as i64 + dy);
                    if q.0 >= 0 && q.1 >= 0 && q.0 < map.width as i64 && q.1 < map.height as i64 && !map.is_land(q.0, q.1) && !probes.contains(&q) {
                        probes.push(q);
                    }
                }
            }
        }
        // Gerade Wasserstücke durch diese Felder, mit Land an beiden Enden.
        let mut crossings: Vec<Crossing> = Vec::new();
        for &(px, py) in &probes {
            for (dx, dy) in [(1i64, 0i64), (0, 1)] {
                let mut a = (px, py);
                while !map.is_land(a.0 - dx, a.1 - dy) && (px - a.0 + py - a.1) < span && a.0 - dx >= 0 && a.1 - dy >= 0 {
                    a = (a.0 - dx, a.1 - dy);
                }
                let mut b = (px, py);
                while !map.is_land(b.0 + dx, b.1 + dy) && (b.0 - a.0 + b.1 - a.1) < span && b.0 + dx < map.width as i64 && b.1 + dy < map.height as i64 {
                    b = (b.0 + dx, b.1 + dy);
                }
                let len = b.0 - a.0 + b.1 - a.1 + 1;
                if len > span || !map.is_land(a.0 - dx, a.1 - dy) || !map.is_land(b.0 + dx, b.1 + dy) {
                    continue;
                }
                let rect = (a.0 as u32, a.1 as u32, if dx == 1 { len as u32 } else { 1 }, if dy == 1 { len as u32 } else { 1 });
                if crossings.iter().any(|c| c.2 == rect) {
                    continue;
                }
                let banks = ((a.0 - dx, a.1 - dy), (b.0 + dx, b.1 + dy));
                crossings.push((banks.0, banks.1, rect));
            }
        }
        // Nahes Ufer zuerst (wo die Welle herkommt), geschätzte Länge des neuen Weges.
        let as32 = |q: (i64, i64)| (q.0 as i32, q.1 as i32);
        let mut scored: Vec<(f32, (i32, i32), Span)> = crossings
            .into_iter()
            .map(|(a, b, rect)| {
                let (a, b) = if dist(as32(a), start) <= dist(as32(b), start) { (as32(a), as32(b)) } else { (as32(b), as32(a)) };
                let len = rect.2.max(rect.3) as f32;
                (dist(start, a) * 1.2 + len + dist(b, goal) * 1.2, a, rect)
            })
            .collect();
        scored.sort_by(|x, y| x.0.total_cmp(&y.0));
        let left = available(world, stock, cmds);
        for (estimate, near_bank, rect) in scored.into_iter().take(10) {
            if reachable && estimate > walk * BRIDGE_GAIN {
                break;
            }
            let len = rect.2.max(rect.3);
            if !world.bridge_cost(len).iter().all(|c| left.get(&c.good).copied().unwrap_or(0) >= c.amount) {
                continue;
            }
            // Kommt man ans nahe Ufer heran?
            let to_bank = nav.find_path(start, near_bank);
            if to_bank.last().is_none_or(|&e| dist(e, near_bank) > 1.5) {
                continue;
            }
            if world.check_bridge(self.player, rect).is_err() {
                continue;
            }
            debug!("Computer {}: baut Brücke {rect:?} (Weg {walk:.0}, mit Brücke etwa {estimate:.0})", self.player);
            self.bridges += 1;
            return Some(ClientMsg::PlaceBridge { x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
        }
        None
    }

    /// Die Wagenschuppen ausbauen (Karren ab Stufe 2), sobald Bretter übrig sind, auch wenn die
    /// Bauliste noch nicht durch ist.
    pub(super) fn upgrade_carts(&self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        // Erst wenn die Wirtschaft läuft (Kaserne steht); vorher braucht er die Bretter für Betriebe.
        if self.count(world, BuildingKind::Barracks) == 0 {
            return;
        }
        let target = self.profile.upgrades.iter().find(|u| u.0 == BuildingKind::CartShed).map_or(0, |u| u.1);
        let Some(def) = world.economy.building(BuildingKind::CartShed) else { return };
        let Some(b) = world.buildings.iter().find(|b| b.owner == self.player && b.kind == BuildingKind::CartShed && b.level < target) else {
            return;
        };
        let Some(next) = def.levels.get(b.level as usize) else { return };
        let reserve = |g: Good| match g {
            Good::Wood => self.profile.wall_wood_reserve,
            Good::Boards => 10,
            _ => 0,
        };
        let left = available(world, stock, cmds);
        if next.cost.iter().all(|c| left.get(&c.good).copied().unwrap_or(0) >= c.amount + reserve(c.good)) && world.check_no_enemy(self.player, b.kind, center(b)).is_ok() {
            cmds.push(ClientMsg::UpgradeBuilding { building_id: b.id });
        }
    }

    /// Betriebe, deren Arbeiter nicht mehr selbst tragen: je Wagenschuppen mit Karren
    /// (ab `CART_LEVEL`) so viele, wie seine Karren schaffen (`PER_CART` je Karren), in
    /// seinem Umkreis, die am weitesten vom nächsten Lager zuerst (dort spart es am meisten).
    pub(super) fn carted(&self, world: &World) -> std::collections::HashSet<u32> {
        let p = self.player;
        let mut out = std::collections::HashSet::new();
        let own = || world.buildings.iter().filter(move |b| b.owner == p);
        let stores: Vec<(f32, f32)> = own().filter(|b| matches!(b.kind, BuildingKind::Keep | BuildingKind::Storehouse | BuildingKind::Granary)).map(center).collect();
        let to_store = |c: (f32, f32)| stores.iter().map(|s| (s.0 - c.0).hypot(s.1 - c.1)).fold(f32::MAX, f32::min);
        let worker = |b: &protocol::BuildingState| {
            world.economy.building(b.kind).is_some_and(|d| d.delivery.worker && d.levels.iter().any(|l| !l.recipes.is_empty()))
        };
        let mut candidates: Vec<(&protocol::BuildingState, f32)> = own().filter(|b| worker(b)).map(|b| (b, to_store(center(b)))).collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        for s in own().filter(|s| s.kind == BuildingKind::CartShed) {
            let Some(def) = world.economy.building(s.kind) else { continue };
            let carts: u32 = def.levels.iter().take(s.level as usize).flat_map(|l| &l.spawns).filter(|sp| sp.kind == protocol::UnitKind::Cart).map(|sp| sp.count).sum();
            if s.level < CART_LEVEL || carts == 0 {
                continue;
            }
            let r = def.at(s.level).service_radius as f32;
            let sc = center(s);
            let mut room = carts * PER_CART;
            for (b, _) in &candidates {
                if room == 0 {
                    break;
                }
                let c = center(b);
                if !out.contains(&b.id) && (r <= 0.0 || (sc.0 - c.0).hypot(sc.1 - c.1) <= r) {
                    out.insert(b.id);
                    room -= 1;
                }
            }
        }
        out
    }

    /// Soll der Arbeiter dieses Betriebs selbst tragen (`carted` siehe oben)? `None` = egal
    /// (kein Betrieb mit Arbeiter).
    pub(super) fn worker_should_carry(&self, world: &World, b: &protocol::BuildingState, carted: &std::collections::HashSet<u32>) -> Option<bool> {
        let def = world.economy.building(b.kind)?;
        if !def.delivery.worker || def.levels.iter().all(|l| l.recipes.is_empty()) {
            return None;
        }
        Some(!carted.contains(&b.id))
    }

    /// Ein Betrieb außerhalb aller Wagenschuppen: dort noch einen (höchstens `CART_SHEDS_MAX`).
    pub(super) fn cart_shed_wanted(&self, world: &World) -> Option<(i64, i64)> {
        let p = self.player;
        let sheds: Vec<&protocol::BuildingState> = world.buildings.iter().filter(|b| b.owner == p && b.kind == BuildingKind::CartShed).collect();
        // Erst wenn der erste ganz ausgebaut ist (zwei Karren); sonst bringt ein zweiter nur Träger.
        if !sheds.iter().any(|s| s.level > CART_LEVEL) || sheds.len() as u32 >= CART_SHEDS_MAX || self.count(world, BuildingKind::Barracks) == 0 {
            return None;
        }
        world
            .buildings
            .iter()
            .filter(|b| b.owner == p)
            .filter(|b| world.economy.building(b.kind).is_some_and(|d| matches!(d.category, BuildingCategory::Raw | BuildingCategory::Food)))
            .map(center)
            .find(|c| {
                sheds.iter().all(|s| {
                    let r = world.economy.building(s.kind).map_or(0, |d| d.at(s.level).service_radius) as f32;
                    let sc = center(s);
                    r > 0.0 && (sc.0 - c.0).hypot(sc.1 - c.1) > r
                })
            })
            .map(|c| (c.0 as i64, c.1 as i64))
    }

    /// Burg fertig, Bauliste durch: nach dem Regler „Weiterbauen“ immer wieder einen Betrieb
    /// mehr (Holz, Wohnungen, Essen, Stein ...) und eine etwas größere Armee.
    pub(super) fn grow(&mut self, world: &World) {
        let g = self.profile.growth;
        if g <= 0.0 || self.clock < self.next_grow || self.grow_round as f32 >= GROW_ROUNDS_MAX * g {
            return;
        }
        self.next_grow = self.clock + 60.0 + 180.0 * (1.0 - g);
        // Nur, was die Stufe bzw. der Charakter überhaupt baut.
        let planned = |k: BuildingKind| self.profile.build_order.iter().any(|e| e.0 == k);
        for step in 0..GROWTH.len() {
            let kind = GROWTH[(self.grow_round as usize + step) % GROWTH.len()];
            if !planned(kind) || world.check_limit(self.player, kind).is_err() {
                continue;
            }
            let have = self.count(world, kind);
            let want = self.extra.get(&kind).copied().unwrap_or(0).max(have) + 1;
            debug!("Computer {}: baut weiter aus, noch ein {kind:?} ({want})", self.player);
            self.extra.insert(kind, want);
            self.grow_round += 1 + step as u32;
            break;
        }
        // Mehr Soldaten, höchstens so viele mehr, wie der Regler vom Profil zulässt.
        let cap = (self.profile.army_size as f32 * g).round() as u32;
        self.grow_army = (self.grow_army + 2).min(cap);
    }

    /// Nach fertiger Burg (Regler „Weiterbauen“): auch Betriebe ausbauen, die nicht in der
    /// Ausbauliste stehen, den billigsten zuerst. Nur, was über der Holzreserve übrig ist.
    pub(super) fn grow_upgrade(&self, world: &World, stock: &StockState, cmds: &mut Vec<ClientMsg>) {
        if self.profile.growth <= 0.0 || self.grow_round == 0 {
            return;
        }
        let wanted = |k: BuildingKind| {
            world.economy.building(k).is_some_and(|d| {
                matches!(d.category, BuildingCategory::Raw | BuildingCategory::Food | BuildingCategory::Workshop | BuildingCategory::Storage | BuildingCategory::Housing | BuildingCategory::Military)
            })
        };
        let reserve = self.profile.wall_wood_reserve;
        let left = available(world, stock, cmds);
        let best = world
            .buildings
            .iter()
            .filter(|b| b.owner == self.player && wanted(b.kind))
            .filter_map(|b| {
                let def = world.economy.building(b.kind)?;
                let next = def.levels.get(b.level as usize)?;
                let ok = next.cost.iter().all(|c| left.get(&c.good).copied().unwrap_or(0) >= c.amount + if c.good == Good::Wood { reserve } else { 0 });
                ok.then(|| (b, next.cost.iter().map(|c| c.amount).sum::<u32>()))
            })
            .min_by_key(|&(b, sum)| (sum, b.id));
        if let Some((b, _)) = best {
            if world.check_no_enemy(self.player, b.kind, center(b)).is_ok() {
                cmds.push(ClientMsg::UpgradeBuilding { building_id: b.id });
            }
        }
    }

    /// Nach einem abgebrochenen Angriff: beim nächsten Mal mit mehr Soldaten, und Essen für sie.
    pub(super) fn attack_failed(&mut self, world: &World, target: u32, power: f32) {
        self.failures = (self.failures + 1).min(4);
        let prev = self.failed_power.get(&target).copied().unwrap_or(0.0);
        self.failed_power.insert(target, prev.max(power));
        self.fail_army = self.profile.army_size * self.failures / 4;
        // Mehr Soldaten essen mehr: eine Obstplantage mehr, und ein Kornspeicher, falls keiner da ist.
        for kind in [BuildingKind::Orchard, BuildingKind::Granary] {
            let have = self.count(world, kind);
            if kind == BuildingKind::Granary && have > 0 {
                continue;
            }
            let want = self.extra.get(&kind).copied().unwrap_or(0).max(have) + 1;
            self.extra.insert(kind, want);
        }
        debug!("Computer {}: Angriff auf {target} abgebrochen ({} Mal), will jetzt {} Soldaten", self.player, self.failures, self.army_target());
    }

    /// Meldung an die Verbündeten (oder zurückziehen mit `None`).
    pub(super) fn post(&self, world: &World, call: Option<(u32, f32, bool)>) {
        let mut board = self.board.lock().unwrap();
        match call {
            Some((target, power, marching)) => {
                board.insert(self.player, AiCall { target, power, marching, at: world.game_seconds() });
            }
            None => {
                board.remove(&self.player);
            }
        }
    }

    /// Frische Meldungen verbündeter Computer gegen Gegner, die er angreifen darf.
    pub(super) fn ally_calls(&self, world: &World) -> Vec<AiCall> {
        let now = world.game_seconds();
        let board = self.board.lock().unwrap();
        let mut calls: Vec<AiCall> = board
            .iter()
            .filter(|&(&ally, c)| {
                ally != self.player
                    && world.allied(self.player, ally)
                    && !world.is_eliminated(ally)
                    && now - c.at <= CALL_FRESH
                    && world.can_attack(self.player, c.target)
                    && !world.is_eliminated(c.target)
            })
            .map(|(_, c)| *c)
            .collect();
        // Wer schon unterwegs ist, zuerst; dann die stärkste Welle.
        calls.sort_by(|a, b| b.marching.cmp(&a.marching).then(b.power.total_cmp(&a.power)).then(a.target.cmp(&b.target)));
        calls
    }
}
