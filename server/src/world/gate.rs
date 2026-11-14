//! Tore und Wehrgänge.
//!
//! Ein Tor ist 3 Felder breit: zwei Pfeiler und in der Mitte der Durchgang. Ist
//! das Tor offen, ist der Durchgang am Boden begehbar, sonst blockiert er wie eine
//! Mauer. Im Modus „automatisch“ schließt es, sobald fremde Einheiten nahe kommen.
//!
//! Oben auf Mauern mit Wehrgang, Türmen und Toren können eigene Einheiten laufen.
//! Hinauf und hinunter geht es nur über die Treppe eines ausgebauten Tores: vom
//! Feld vor oder hinter dem Durchgang auf die Tormitte oben.

use std::collections::{HashMap, HashSet, VecDeque};

use protocol::{BuildingKind, BuildingState, GateMode};

use super::World;

/// Wegpunkte mit Ebene: `true` = oben auf der Mauer.
pub(super) type Route = (Vec<(f32, f32)>, Vec<bool>);

/// Zum Wieder-Öffnen müssen fremde Einheiten etwas weiter weg sein als zum Schließen,
/// sonst klappert das Tor auf und zu.
const REOPEN_EXTRA: f32 = 2.0;

fn center(t: (i64, i64)) -> (f32, f32) {
    (t.0 as f32 + 0.5, t.1 as f32 + 0.5)
}

/// Mittleres Feld eines Gebäudes (beim Tor der Durchgang).
pub fn passage(b: &BuildingState) -> (u32, u32) {
    (b.x + b.w / 2, b.y + b.h / 2)
}

/// Blockiert dieses Feld des Gebäudes den Boden? Nur der offene Durchgang eines Tores nicht.
pub fn blocks(b: &BuildingState, tx: u32, ty: u32) -> bool {
    !(b.kind == BuildingKind::Gate && b.gate_open && passage(b) == (tx, ty))
}

/// Die Felder vor und hinter dem Durchgang (Fuß der Treppe).
fn entries(b: &BuildingState) -> [(i64, i64); 2] {
    let (px, py) = passage(b);
    let (px, py) = (px as i64, py as i64);
    if b.w >= b.h {
        [(px, b.y as i64 - 1), (px, (b.y + b.h) as i64)]
    } else {
        [(b.x as i64 - 1, py), ((b.x + b.w) as i64, py)]
    }
}

impl World {
    /// Tore öffnen oder schließen, je nach Modus und fremden Einheiten in der Nähe.
    pub(super) fn step_gates(&mut self) {
        let mut changes = Vec::new();
        for (bi, b) in self.buildings.iter().enumerate() {
            if b.kind != BuildingKind::Gate {
                continue;
            }
            let want = match b.settings.gate {
                GateMode::Open => true,
                GateMode::Closed => false,
                GateMode::Auto => {
                    let r = self.economy.building(b.kind).map_or(0, |d| d.gate_radius) as f32;
                    let r = if b.gate_open { r } else { r + REOPEN_EXTRA };
                    let (px, py) = passage(b);
                    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                    !self.units.iter().any(|u| {
                        u.owner != b.owner && (u.pos.0 - cx).powi(2) + (u.pos.1 - cy).powi(2) <= r * r
                    })
                }
            };
            if want != b.gate_open {
                changes.push((bi, want));
            }
        }
        if changes.is_empty() {
            return;
        }
        for (bi, open) in changes {
            self.set_gate_open(bi, open);
        }
        self.replan_all();
    }

    fn set_gate_open(&mut self, bi: usize, open: bool) {
        let b = &mut self.buildings[bi];
        b.gate_open = open;
        let id = b.id;
        let (px, py) = passage(b);
        self.map.nav.set_blocked(px as i32, py as i32, !open);
        if !open {
            // Wer im Durchgang steht, wird hinausgeschoben.
            let t = (py * self.map.width + px) as usize;
            let inside: Vec<usize> = (0..self.units.len())
                .filter(|&k| !self.units[k].on_wall && self.tile_of(self.units[k].pos) == Some(t))
                .collect();
            for k in inside {
                self.push_out(k);
            }
        }
        self.dirty_buildings.insert(id);
    }

    fn tile_of(&self, p: (f32, f32)) -> Option<usize> {
        let (x, y) = (p.0.floor() as i64, p.1.floor() as i64);
        (x >= 0 && y >= 0 && x < self.map.width as i64 && y < self.map.height as i64)
            .then(|| (y as u32 * self.map.width + x as u32) as usize)
    }

    /// Einheit auf das nächste begehbare Bodenfeld stellen (sie stand auf etwas,
    /// das jetzt blockiert oder weg ist). Ihr Ziel behält sie.
    pub(super) fn push_out(&mut self, k: usize) {
        let pos = self.units[k].pos;
        let (ux, uy) = (pos.0.floor() as i64, pos.1.floor() as i64);
        let mut best: Option<(f32, (i64, i64))> = None;
        for r in 0..=6i64 {
            for y in uy - r..=uy + r {
                for x in ux - r..=ux + r {
                    if (x - ux).abs().max((y - uy).abs()) != r || !self.map.nav.is_open(x as i32, y as i32) {
                        continue;
                    }
                    let c = center((x, y));
                    let d = (c.0 - pos.0).powi(2) + (c.1 - pos.1).powi(2);
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, (x, y)));
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        let u = &mut self.units[k];
        u.on_wall = false;
        u.layers.clear();
        if let Some((_, t)) = best {
            u.pos = center(t);
        }
        // Mit Ziel: neu planen (replan_all braucht einen nicht leeren Weg).
        u.path = if u.target.is_some() { vec![u.pos] } else { Vec::new() };
    }

    /// Felder, auf denen Einheiten dieses Spielers oben laufen können.
    fn walk_tiles(&self, owner: u32) -> HashSet<usize> {
        let mut set = HashSet::new();
        for b in self.buildings.iter().filter(|b| b.owner == owner) {
            let Some(def) = self.economy.building(b.kind) else { continue };
            if def.walkway_from_level == 0 || b.level < def.walkway_from_level {
                continue;
            }
            for ty in b.y..b.y + b.h {
                for tx in b.x..b.x + b.w {
                    set.insert((ty * self.map.width + tx) as usize);
                }
            }
        }
        set
    }

    /// Ist hier oben ein Wehrgang dieses Spielers?
    pub(super) fn is_walkway(&self, owner: u32, p: (f32, f32)) -> bool {
        let Some(t) = self.tile_of(p) else { return false };
        let id = self.occupied[t];
        id != 0
            && self.buildings.iter().find(|b| b.id == id).is_some_and(|b| {
                b.owner == owner
                    && self.economy.building(b.kind).is_some_and(|d| d.walkway_from_level > 0 && b.level >= d.walkway_from_level)
            })
    }

    /// Treppen dieses Spielers: (Feld oben, Felder am Boden davor).
    /// Tor: Tormitte, vom Feld vor oder hinter dem Durchgang. Andere (Treppenhaus,
    /// Turm): Gebäudemitte, von jedem freien Feld direkt neben dem Gebäude.
    fn stairs(&self, owner: u32) -> Vec<(usize, Vec<(i64, i64)>)> {
        let open = |&(x, y): &(i64, i64)| self.map.nav.is_open(x as i32, y as i32);
        self.buildings
            .iter()
            .filter(|b| b.owner == owner)
            .filter(|b| self.economy.building(b.kind).is_some_and(|d| d.stairs_from_level > 0 && b.level >= d.stairs_from_level))
            .map(|b| {
                let (cx, cy) = passage(b);
                let feet: Vec<(i64, i64)> = if b.kind == BuildingKind::Gate {
                    entries(b).into_iter().filter(open).collect()
                } else {
                    let (x0, y0, x1, y1) = (b.x as i64, b.y as i64, (b.x + b.w) as i64, (b.y + b.h) as i64);
                    (x0..x1)
                        .flat_map(|x| [(x, y0 - 1), (x, y1)])
                        .chain((y0..y1).flat_map(|y| [(x0 - 1, y), (x1, y)]))
                        .filter(open)
                        .collect()
                };
                ((cy * self.map.width + cx) as usize, feet)
            })
            .collect()
    }

    /// Breitensuche oben auf dem Wehrgang ab `from`: Vorgänger je erreichbarem Feld.
    /// Schräg nur, wenn beide Felder um die Ecke auch begehbar sind; Höhenunterschied höchstens 1.
    fn wall_tree(&self, tiles: &HashSet<usize>, from: usize) -> HashMap<usize, usize> {
        let w = self.map.width as i64;
        let h = self.map.height as i64;
        let mut came = HashMap::new();
        if !tiles.contains(&from) {
            return came;
        }
        came.insert(from, from);
        let mut queue = VecDeque::from([from]);
        while let Some(t) = queue.pop_front() {
            let (x, y) = (t as i64 % w, t as i64 / w);
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let n = (ny * w + nx) as usize;
                if came.contains_key(&n) || !tiles.contains(&n) {
                    continue;
                }
                if dx != 0 && dy != 0 && !(tiles.contains(&((y * w + nx) as usize)) && tiles.contains(&((ny * w + x) as usize))) {
                    continue;
                }
                if self.map.levels[t].abs_diff(self.map.levels[n]) > 1 {
                    continue;
                }
                came.insert(n, t);
                queue.push_back(n);
            }
        }
        came
    }

    /// Felder von der Wurzel des Baums bis `to` (ohne Wurzel), als Feldmitten.
    fn wall_points(&self, came: &HashMap<usize, usize>, to: usize) -> Vec<(f32, f32)> {
        let w = self.map.width as usize;
        let mut tiles = Vec::new();
        let mut t = to;
        while let Some(&prev) = came.get(&t) {
            if prev == t {
                break;
            }
            tiles.push(t);
            t = prev;
        }
        tiles.reverse();
        tiles.into_iter().map(|t| center(((t % w) as i64, (t / w) as i64))).collect()
    }

    /// Bodenweg zu einem Feld, nur wenn er es auch erreicht.
    fn ground_to(&self, from: (f32, f32), to: (i64, i64)) -> Option<Vec<(f32, f32)>> {
        let goal = center(to);
        let pts = self.map.nav.waypoints(from, goal);
        let end = pts.last().copied().unwrap_or(from);
        ((end.0.floor() as i64, end.1.floor() as i64) == to).then_some(pts)
    }

    /// Weg für eine Einheit, auch über Wehrgänge und Treppen.
    pub(super) fn plan(&self, owner: u32, from: (f32, f32), from_wall: bool, to: (f32, f32), to_wall: bool) -> Route {
        let ground = |pts: Vec<(f32, f32)>| {
            let n = pts.len();
            (pts, vec![false; n])
        };
        if !from_wall && !to_wall {
            return ground(self.map.nav.waypoints(from, to));
        }
        let tiles = self.walk_tiles(owner);
        let stairs = self.stairs(owner);
        let mut route: Route = (Vec::new(), Vec::new());
        let push = |r: &mut Route, pts: Vec<(f32, f32)>, wall: bool| {
            r.1.extend(std::iter::repeat_n(wall, pts.len()));
            r.0.extend(pts);
        };

        // Oben: erst bis zu einer Treppe (oder gleich zum Ziel), dann hinunter.
        let mut pos = from;
        if from_wall {
            let Some(start) = self.tile_of(from) else { return route };
            let tree = self.wall_tree(&tiles, start);
            if to_wall {
                if let Some(goal) = self.tile_of(to).filter(|g| tree.contains_key(g)) {
                    push(&mut route, self.wall_points(&tree, goal), true);
                    push(&mut route, vec![to], true);
                    return route;
                }
            }
            // Treppe, deren Fuß dem Ziel am nächsten ist.
            let down = stairs
                .iter()
                .filter(|(top, _)| tree.contains_key(top))
                .flat_map(|(top, feet)| feet.iter().map(move |&f| (*top, f)))
                .min_by(|a, b| dist2(center(a.1), to).total_cmp(&dist2(center(b.1), to)));
            let Some((top, foot)) = down else {
                return route; // kein Weg hinunter: stehen bleiben
            };
            push(&mut route, self.wall_points(&tree, top), true);
            push(&mut route, vec![center(foot)], false);
            pos = center(foot);
        }
        if !to_wall {
            push(&mut route, self.map.nav.waypoints(pos, to), false);
            return route;
        }

        // Unten: zur Treppe, die zum Ziel oben führt.
        let Some(goal) = self.tile_of(to) else { return route };
        let tree = self.wall_tree(&tiles, goal);
        let mut ups: Vec<(usize, (i64, i64))> = stairs
            .iter()
            .filter(|(top, _)| tree.contains_key(top))
            .flat_map(|(top, feet)| feet.iter().map(move |&f| (*top, f)))
            .collect();
        ups.sort_by(|a, b| dist2(center(a.1), pos).total_cmp(&dist2(center(b.1), pos)));
        let best = ups
            .iter()
            .take(4)
            .filter_map(|&(top, foot)| {
                let pts = if (pos.0.floor() as i64, pos.1.floor() as i64) == foot {
                    Vec::new()
                } else {
                    self.ground_to(pos, foot)?
                };
                Some((path_len(pos, &pts), top, pts))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, top, pts)) = best else {
            // Keine erreichbare Treppe: so nah wie möglich heran.
            push(&mut route, self.map.nav.waypoints(pos, to), false);
            return route;
        };
        push(&mut route, pts, false);
        // Treppe hinauf, dann oben zum Ziel (der Baum wurzelt am Ziel, also umgekehrt).
        let w = self.map.width as usize;
        let mut up = vec![center(((top % w) as i64, (top / w) as i64))];
        let mut t = top;
        while let Some(&prev) = tree.get(&t) {
            if prev == t {
                break;
            }
            up.push(center(((prev % w) as i64, (prev / w) as i64)));
            t = prev;
        }
        up.push(to);
        push(&mut route, up, true);
        route
    }

    /// Stehen Einheiten oben auf Feldern, die keinen Wehrgang mehr haben, kommen sie herunter.
    pub(super) fn drop_from_walls(&mut self) {
        let fallen: Vec<usize> = (0..self.units.len())
            .filter(|&k| {
                let u = &self.units[k];
                u.on_wall && !self.is_walkway(u.owner, u.pos)
            })
            .collect();
        for k in fallen {
            self.push_out(k);
        }
    }
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

fn path_len(from: (f32, f32), pts: &[(f32, f32)]) -> f32 {
    let mut len = 0.0;
    let mut cur = from;
    for &p in pts {
        len += dist2(cur, p).sqrt();
        cur = p;
    }
    len
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, GateMode, Good, GoodAmount, UnitKind};

    use crate::map::GameMap;
    use crate::world::{Unit, World};

    /// Flache Karte, quer bei y = 10 eine Mauer mit Tor in der Mitte (x 10..12).
    fn walled(wall_level: u8, gate_level: u8) -> World {
        let row = "0".repeat(24);
        let text = format!("spawn 2 2\n{}", format!("{row}\n").repeat(24));
        let eco = crate::economy::Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        let lots: Vec<GoodAmount> =
            [Good::Wood, Good::Stone, Good::Boards, Good::Blocks].iter().map(|&good| GoodAmount { good, amount: 5000 }).collect();
        w.admin_give(1, &lots, 0);
        for x in (0..24).filter(|x| !(10..13).contains(x)) {
            w.place_building_rotated(1, BuildingKind::Wall, x, 10, 0, wall_level).unwrap();
        }
        w.place_building_rotated(1, BuildingKind::Gate, 10, 10, 0, gate_level).unwrap();
        w
    }

    fn gate(w: &World) -> usize {
        w.buildings.iter().position(|b| b.kind == BuildingKind::Gate).unwrap()
    }

    fn king(w: &World) -> usize {
        w.units.iter().position(|u| u.kind == UnitKind::King && u.owner == 1).unwrap()
    }

    fn run(w: &mut World, s: f32) {
        for _ in 0..(s * 20.0) as usize {
            w.step(0.05);
        }
    }

    #[test]
    fn tor_schliesst_bei_gegnern_und_oeffnet_wieder() {
        let mut w = walled(1, 1);
        let g = gate(&w);
        assert!(w.buildings[g].gate_open && w.map.nav.is_open(11, 10), "neues Tor ist offen");
        w.units.push(Unit::new(999, 2, UnitKind::King, (11.5, 16.5)));
        run(&mut w, 0.1);
        assert!(!w.buildings[g].gate_open, "Gegner in der Nähe: zu");
        assert!(!w.map.nav.is_open(11, 10));
        let k = w.units.len() - 1;
        w.units[k].pos = (20.5, 23.5);
        run(&mut w, 0.1);
        assert!(w.buildings[g].gate_open, "Gegner weg: wieder offen");
        assert!(w.map.nav.is_open(11, 10));
    }

    #[test]
    fn fest_zu_und_fest_offen() {
        let mut w = walled(1, 1);
        let g = gate(&w);
        let id = w.buildings[g].id;
        let mut s = w.buildings[g].settings.clone();
        s.gate = GateMode::Closed;
        w.configure(1, id, s.clone()).unwrap();
        run(&mut w, 0.1);
        assert!(!w.map.nav.is_open(11, 10));
        // Der König kommt nicht hindurch, er bleibt diesseits stehen.
        let k = king(&w);
        w.order_move(1, &[w.units[k].id], (11.5, 20.5));
        run(&mut w, 30.0);
        assert!(w.units[k].pos.1 < 10.0, "Tor zu: König bei {:?}", w.units[k].pos);
        // Fest offen bleibt offen, auch wenn ein Gegner davor steht.
        s.gate = GateMode::Open;
        w.configure(1, id, s).unwrap();
        w.units.push(Unit::new(999, 2, UnitKind::King, (11.5, 14.5)));
        w.order_move(1, &[w.units[k].id], (11.5, 20.5));
        run(&mut w, 30.0);
        assert!(w.buildings[g].gate_open);
        assert!(w.units[k].pos.1 > 19.0, "Tor offen: König bei {:?}", w.units[k].pos);
    }

    #[test]
    fn wer_im_tor_steht_wird_hinausgeschoben() {
        let mut w = walled(1, 1);
        let k = king(&w);
        w.units[k].pos = (11.5, 10.5);
        w.units.push(Unit::new(999, 2, UnitKind::King, (11.5, 15.5)));
        run(&mut w, 0.1);
        let p = w.units[k].pos;
        assert!(w.map.nav.is_open(p.0.floor() as i32, p.1.floor() as i32), "König steht frei: {p:?}");
        assert_ne!((p.0.floor(), p.1.floor()), (11.0, 10.0));
    }

    #[test]
    fn ueber_die_treppe_auf_den_wehrgang_und_zurueck() {
        let mut w = walled(2, 2);
        let k = king(&w);
        let id = w.units[k].id;
        w.order_move(1, &[id], (4.5, 10.5));
        run(&mut w, 40.0);
        assert!(w.units[k].on_wall, "König steht oben");
        assert!((w.units[k].pos.0 - 4.5).abs() < 0.01 && (w.units[k].pos.1 - 10.5).abs() < 0.01, "{:?}", w.units[k].pos);
        assert!(w.snapshot()[k].on_wall);
        w.order_move(1, &[id], (20.5, 16.5));
        run(&mut w, 40.0);
        assert!(!w.units[k].on_wall, "wieder unten");
        assert!((w.units[k].pos.0 - 20.5).abs() < 0.01 && (w.units[k].pos.1 - 16.5).abs() < 0.01, "{:?}", w.units[k].pos);
    }

    #[test]
    fn ohne_treppe_kein_weg_nach_oben_und_abriss_holt_herunter() {
        let mut w = walled(2, 1);
        let k = king(&w);
        let id = w.units[k].id;
        w.order_move(1, &[id], (4.5, 10.5));
        run(&mut w, 40.0);
        assert!(!w.units[k].on_wall, "ohne Treppe bleibt der König unten");
        // Mit Treppe hinauf, dann wird die Mauer unter ihm abgerissen.
        let g = gate(&w);
        let gid = w.buildings[g].id;
        w.upgrade(1, gid).unwrap();
        w.order_move(1, &[id], (4.5, 10.5));
        run(&mut w, 40.0);
        assert!(w.units[k].on_wall);
        let wall = w.buildings.iter().find(|b| b.x == 4 && b.y == 10).unwrap().id;
        w.demolish(1, wall).unwrap();
        assert!(!w.units[k].on_wall, "Mauer weg: König unten");
        let p = w.units[k].pos;
        assert!(w.map.nav.is_open(p.0.floor() as i32, p.1.floor() as i32));
    }

    #[test]
    fn treppenhaus_und_turm_mit_treppe() {
        let mut w = walled(2, 1);
        let k = king(&w);
        let id = w.units[k].id;
        // Treppenhaus direkt an der Mauer.
        w.place_building_rotated(1, BuildingKind::Staircase, 5, 11, 0, 1).unwrap();
        w.order_move(1, &[id], (15.5, 10.5));
        run(&mut w, 40.0);
        assert!(w.units[k].on_wall, "über das Treppenhaus hinauf");
        assert!((w.units[k].pos.0 - 15.5).abs() < 0.01, "{:?}", w.units[k].pos);
        // Turm ohne Treppe: kein Aufgang; mit Treppe (Stufe 2) schon.
        let mut w = walled(2, 1);
        let k = king(&w);
        let id = w.units[k].id;
        let tower = w.place_building_rotated(1, BuildingKind::Tower, 18, 11, 0, 1).unwrap().id;
        w.order_move(1, &[id], (2.5, 10.5));
        run(&mut w, 40.0);
        assert!(!w.units[k].on_wall, "Turm Stufe 1 hat keine Treppe");
        w.upgrade(1, tower).unwrap();
        w.order_move(1, &[id], (2.5, 10.5));
        run(&mut w, 60.0);
        assert!(w.units[k].on_wall, "über den Turm hinauf");
        assert!((w.units[k].pos.0 - 2.5).abs() < 0.01, "{:?}", w.units[k].pos);
    }

    #[test]
    fn tor_im_spielstand() {
        let mut w = walled(1, 2);
        w.units.push(Unit::new(999, 2, UnitKind::King, (11.5, 14.5)));
        run(&mut w, 0.1);
        let save = w.to_save("test");
        let text = serde_json::to_string(&save).unwrap();
        let back: crate::world::SaveGame = serde_json::from_str(&text).unwrap();
        let map = GameMap::parse(&format!("spawn 2 2\n{}", format!("{}\n", "0".repeat(24)).repeat(24))).unwrap();
        let l = World::from_save(map, w.economy.clone(), back).unwrap();
        assert!(!l.map.nav.is_open(11, 10), "geschlossenes Tor bleibt zu");
        assert!(!l.map.nav.is_open(10, 10), "Pfeiler blockiert");
    }
}
