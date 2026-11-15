//! Wo der Computergegner was hinbaut.
//!
//! Um den Bergfried liegt ein (später gebauter) Mauerring mit einem Tor in der Mitte
//! jeder Seite. Von Anfang an bleiben frei: der Ring selbst mit einem Feld Rand und
//! drei Felder breite Straßen vom Bergfried zu den Toren. Wohnhäuser, Lager und
//! Werkstätten kommen nach innen, Holzfäller, Steinbruch und Obstplantage nach außen.
//! Zwischen Gebäuden bleibt immer ein Feld Weg.

use protocol::{BuildingKind, BuildingState, ResourceKind};

use crate::world::World;

/// Wie weit vom Ankerpunkt er höchstens nach einem Bauplatz sucht.
const SEARCH_RADIUS: i64 = 90;
/// So weit um den Bergfried herum sucht er erreichbare Felder.
const REACH_RADIUS: i64 = 110;

/// Rechteck in Feldern, Ecken eingeschlossen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl Rect {
    fn of(b: &BuildingState) -> Rect {
        Rect { x0: b.x as i64, y0: b.y as i64, x1: (b.x + b.w) as i64 - 1, y1: (b.y + b.h) as i64 - 1 }
    }

    fn grow(self, n: i64) -> Rect {
        Rect { x0: self.x0 - n, y0: self.y0 - n, x1: self.x1 + n, y1: self.y1 + n }
    }

    fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// So nah rückt der Mauerring mindestens an den Bergfried heran.
const MIN_GAP: i64 = 5;

/// Wohin ein Gebäude soll.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zone {
    Inside,
    Outside,
}

/// Grundriss der Siedlung eines Computergegners.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Linie des Mauerrings.
    pub ring: Rect,
    pub mid: (i64, i64),
}

/// Ein Tor im Ring: oben links, Drehung.
pub type GateSpot = (u32, u32, u8);

impl Layout {
    /// Grundriss um den Bergfried. Jede Seite des Rings liegt höchstens `gap` Felder
    /// vom Bergfried weg, näher dran, wenn dort sonst Wasser oder Klippen wären
    /// (etwa ein Burggraben: dann steht die Mauer an seinem Rand).
    pub fn new(world: &World, keep: &BuildingState, gap: u32) -> Layout {
        let k = Rect::of(keep);
        let gap = (gap as i64).max(MIN_GAP);
        let start = keep.entrance.map_or((k.x0, k.y1 + 1), |(x, y)| (x as i64, y as i64));
        // Nur das Gelände zählt (nicht Gebäude oder Wald), damit der Grundriss gleich bleibt,
        // egal was schon steht, auch nach dem Laden eines Spiels.
        let reach = Reach::terrain(world, start);
        let mid = ((k.x0 + k.x1) / 2, (k.y0 + k.y1) / 2);
        // Je Seite: so weit kommt man vom Bergfried geradeaus, bevor Wasser oder eine Klippe
        // kommt. Der Ring bleibt davor, damit er nicht über einen Graben springt.
        let limit = |side: usize| -> i64 {
            let lanes = match side {
                0 | 1 => [k.x0, mid.0, k.x1],
                _ => [k.y0, mid.1, k.y1],
            };
            let mut limit = gap;
            for lane in lanes {
                for t in 1..=gap {
                    let (x, y) = match side {
                        0 => (lane, k.y1 + t),
                        1 => (lane, k.y0 - t),
                        2 => (k.x0 - t, lane),
                        _ => (k.x1 + t, lane),
                    };
                    if !reach.at(x, y) {
                        limit = limit.min(t - 1);
                        break;
                    }
                }
            }
            limit.max(1)
        };
        let mut ring = k.grow(gap);
        // Zwei Runden, weil jede Seite die Länge der anderen bestimmt.
        for _ in 0..2 {
            for side in 0..4 {
                let max = limit(side);
                let mut best = (i64::MAX, max);
                for d in MIN_GAP.min(max)..=max {
                    let mut r = ring;
                    match side {
                        0 => r.y1 = k.y1 + d,
                        1 => r.y0 = k.y0 - d,
                        2 => r.x0 = k.x0 - d,
                        _ => r.x1 = k.x1 + d,
                    }
                    let line: Vec<(i64, i64)> = match side {
                        0 => (r.x0..=r.x1).map(|x| (x, r.y1)).collect(),
                        1 => (r.x0..=r.x1).map(|x| (x, r.y0)).collect(),
                        2 => (r.y0..=r.y1).map(|y| (r.x0, y)).collect(),
                        _ => (r.y0..=r.y1).map(|y| (r.x1, y)).collect(),
                    };
                    let blocked = line.iter().filter(|&&(x, y)| !reach.at(x, y)).count() as i64;
                    // Felder, die man vom Bergfried aus nicht erreicht (Wasser, Klippe, jenseits
                    // des Grabens), zählen mehr als ein großer Ring.
                    let score = blocked * 4 - d;
                    if score < best.0 {
                        best = (score, d);
                    }
                }
                match side {
                    0 => ring.y1 = k.y1 + best.1,
                    1 => ring.y0 = k.y0 - best.1,
                    2 => ring.x0 = k.x0 - best.1,
                    _ => ring.x1 = k.x1 + best.1,
                }
            }
        }
        Layout { ring, mid }
    }

    /// Straßen vom Bergfried zu den vier Toren (3 Felder breit, bis 3 Felder vor den Ring).
    fn on_street(&self, x: i64, y: i64) -> bool {
        let outer = self.ring.grow(3);
        let vertical = (x - self.mid.0).abs() <= 1 && y >= outer.y0 && y <= outer.y1;
        let horizontal = (y - self.mid.1).abs() <= 1 && x >= outer.x0 && x <= outer.x1;
        vertical || horizontal
    }

    /// Felder, auf denen in dieser Zone ein Gebäude (ohne Mauern) stehen darf:
    /// nicht auf Ring und Straßen, mit einem Feld Abstand zu allen Gebäuden.
    fn free_tiles(&self, world: &World, zone: Zone) -> Vec<bool> {
        let (w, h) = (world.map.width as i64, world.map.height as i64);
        // Ring mit einem Feld Rand drinnen und zwei draußen.
        let inner = self.ring.grow(-2);
        let outer = self.ring.grow(2);
        let mut free = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let ok = match zone {
                    Zone::Inside => inner.contains(x, y),
                    Zone::Outside => !outer.contains(x, y),
                };
                free[(y * w + x) as usize] = ok && !self.on_street(x, y);
            }
        }
        for b in &world.buildings {
            let r = Rect::of(b).grow(1);
            for y in r.y0.max(0)..=r.y1.min(h - 1) {
                for x in r.x0.max(0)..=r.x1.min(w - 1) {
                    free[(y * w + x) as usize] = false;
                }
            }
        }
        free
    }

    /// Die Felder der Ringlinie, Seite für Seite.
    pub fn ring_tiles(&self) -> Vec<(i64, i64)> {
        let r = self.ring;
        let mut tiles = Vec::new();
        for x in r.x0..=r.x1 {
            tiles.push((x, r.y0));
            tiles.push((x, r.y1));
        }
        for y in r.y0 + 1..r.y1 {
            tiles.push((r.x0, y));
            tiles.push((r.x1, y));
        }
        tiles
    }

    /// Wo die Tore hinkommen: Mitte jeder Seite (Süd, Nord, West, Ost).
    pub fn gates(&self) -> [GateSpot; 4] {
        let r = self.ring;
        let (mx, my) = self.mid;
        let p = |v: i64| v.max(0) as u32;
        [(p(mx - 1), p(r.y1), 0), (p(mx - 1), p(r.y0), 2), (p(r.x0), p(my - 1), 1), (p(r.x1), p(my - 1), 3)]
    }

    /// Gehört dieses Feld zu einem Tor (dort keine Mauer setzen)?
    pub fn is_gate_tile(&self, x: i64, y: i64) -> bool {
        self.gates().iter().any(|&(gx, gy, rot)| {
            let (w, h) = if rot % 2 == 1 { (1, 3) } else { (3, 1) };
            Rect { x0: gx as i64, y0: gy as i64, x1: gx as i64 + w - 1, y1: gy as i64 + h - 1 }.contains(x, y)
        })
    }
}

/// Nächstes Feld mit einer dieser Ressourcen, vom Punkt `from` aus, außerhalb des Rings bevorzugt.
fn nearest_resource(world: &World, from: (i64, i64), want: &[ResourceKind], layout: &Layout, reach: &Reach) -> Option<(i64, i64)> {
    let map = &world.map;
    let mut best: Option<((i64, i64), i64)> = None;
    for r in 0..=SEARCH_RADIUS {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs().max(dy.abs()) != r {
                    continue;
                }
                let (x, y) = (from.0 + dx, from.1 + dy);
                if x < 0 || y < 0 || x >= map.width as i64 || y >= map.height as i64 {
                    continue;
                }
                if !want.contains(&map.resources[(y as u32 * map.width + x as u32) as usize]) {
                    continue;
                }
                // Nur Vorkommen, an die seine Leute herankommen.
                if ![(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|&(nx, ny)| reach.at(x + nx, y + ny)) {
                    continue;
                }
                // Felder im Ring zählen weniger: dort baut er keine Abbaubetriebe hin.
                let score = r + if layout.ring.grow(2).contains(x, y) { 12 } else { 0 };
                if best.is_none_or(|(_, s)| score < s) {
                    best = Some(((x, y), score));
                }
            }
        }
        if best.is_some_and(|(_, s)| s <= r) {
            break;
        }
    }
    best.map(|(p, _)| p)
}

/// Bauplatz für ein Gebäude (keine Mauern/Tore): oben links und Drehung.
/// Prüft mit denselben Regeln wie beim echten Bauen und ob der Eingang vom Bergfried
/// aus erreichbar ist.
/// `near`: dort in der Nähe statt am üblichen Ort (Bergfried bzw. nächstes Vorkommen), dann
/// bevorzugt außerhalb des Rings.
pub fn find_site(
    world: &World,
    player: u32,
    kind: BuildingKind,
    layout: &Layout,
    near: Option<(i64, i64)>,
) -> Option<(u32, u32, u8)> {
    let def = world.economy.building(kind)?;
    let mut want: Vec<ResourceKind> = def.gathers.clone();
    if let Some(p) = &def.placement {
        want.extend(p.resources.iter().copied());
    }
    // Was ein Vorkommen braucht (Holzfäller, Steinbruch, Acker), kommt dorthin, also nach
    // außen; alles andere möglichst nah an den Bergfried, in den Ring.
    // Soll es neben etwas Bestimmtes, dann auf dieselbe Seite des Rings.
    let zone = match near {
        Some((x, y)) if layout.ring.grow(-2).contains(x, y) => Zone::Inside,
        Some(_) => Zone::Outside,
        None if !want.is_empty() => Zone::Outside,
        None => Zone::Inside,
    };
    let start = world
        .buildings
        .iter()
        .find(|b| b.owner == player && b.kind == BuildingKind::Keep)
        .and_then(|b| b.entrance)?;
    let reach = Reach::from(world, (start.0 as i64, start.1 as i64));
    let anchor = if let Some(p) = near {
        p
    } else if want.is_empty() {
        layout.mid
    } else {
        nearest_resource(world, layout.mid, &want, layout, &reach)?
    };
    let (mw, mh) = (world.map.width as i64, world.map.height as i64);
    // Erst die gewünschte Zone, dann (falls dort nichts passt) überall außer Ring und Straßen.
    for zone in [zone, if zone == Zone::Inside { Zone::Outside } else { Zone::Inside }] {
        let free = layout.free_tiles(world, zone);
        let fits = |r: &Rect| {
            r.x0 >= 0
                && r.y0 >= 0
                && r.x1 < mw
                && r.y1 < mh
                && (r.y0..=r.y1).all(|y| (r.x0..=r.x1).all(|x| free[(y * mw + x) as usize]))
        };
        for r in 0..=SEARCH_RADIUS {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    for rotation in rotations_towards(anchor, layout.mid) {
                        let (w, h) = if rotation % 2 == 1 { (def.h, def.w) } else { (def.w, def.h) };
                        let x = anchor.0 + dx - w as i64 / 2;
                        let y = anchor.1 + dy - h as i64 / 2;
                        if x < 0 || y < 0 {
                            continue;
                        }
                        let rect = Rect { x0: x, y0: y, x1: x + w as i64 - 1, y1: y + h as i64 - 1 };
                        if !fits(&rect) {
                            continue;
                        }
                        if world.check_placement(player, kind, (x as u32, y as u32), rotation, 1).is_err() {
                            continue;
                        }
                        let (ex, ey) = entrance_of(&rect, rotation);
                        if reach.at(ex as i64, ey as i64) {
                            return Some((x as u32, y as u32, rotation));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Drehungen, die den Eingang zum Bergfried zeigen lassen, die beste zuerst.
fn rotations_towards(from: (i64, i64), to: (i64, i64)) -> [u8; 4] {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    // 0 = Eingang unten (+y), 1 = links (-x), 2 = oben (-y), 3 = rechts (+x).
    let first = if dx.abs() > dy.abs() {
        if dx > 0 { 3 } else { 1 }
    } else if dy < 0 {
        2
    } else {
        0
    };
    [first, (first + 1) % 4, (first + 3) % 4, (first + 2) % 4]
}

/// Feld vor dem Eingang (wie `world::entrance_tile`).
fn entrance_of(r: &Rect, rotation: u8) -> (i32, i32) {
    let (x, y, w, h) = (r.x0, r.y0, r.x1 - r.x0 + 1, r.y1 - r.y0 + 1);
    let (ex, ey) = match rotation % 4 {
        0 => (x + w / 2, y + h),
        1 => (x - 1, y + h / 2),
        2 => (x + (w - 1) / 2, y - 1),
        _ => (x + w, y + (h - 1) / 2),
    };
    (ex as i32, ey as i32)
}

/// Welche Felder vom Bergfried aus zu Fuß erreichbar sind (im Umkreis `REACH_RADIUS`).
struct Reach {
    x0: i64,
    y0: i64,
    size: i64,
    seen: Vec<bool>,
}

impl Reach {
    /// Was man jetzt zu Fuß erreicht (Gebäude, Wald und Felsen sperren).
    fn from(world: &World, start: (i64, i64)) -> Reach {
        let nav = &world.map.nav;
        Reach::flood(start, nav.is_open(start.0 as i32, start.1 as i32), |x, y, dx, dy| {
            nav.can_step(x as i32, y as i32, dx as i32, dy as i32)
        })
    }

    /// Was man über das Gelände erreicht: kein Wasser, höchstens eine Stufe zum Nachbarn.
    fn terrain(world: &World, start: (i64, i64)) -> Reach {
        let map = &world.map;
        let (w, h) = (map.width as i64, map.height as i64);
        let tile = |x: i64, y: i64| -> Option<usize> {
            (x >= 0 && y >= 0 && x < w && y < h).then(|| (y * w + x) as usize)
        };
        let open = |i: usize| map.tiles[i] != protocol::TileKind::Water;
        Reach::flood(start, tile(start.0, start.1).is_some_and(open), |x, y, dx, dy| {
            match (tile(x, y), tile(x + dx, y + dy)) {
                (Some(a), Some(b)) => open(a) && open(b) && map.levels[a].abs_diff(map.levels[b]) <= 1,
                _ => false,
            }
        })
    }

    fn flood(start: (i64, i64), start_open: bool, step: impl Fn(i64, i64, i64, i64) -> bool) -> Reach {
        let size = 2 * REACH_RADIUS + 1;
        let (x0, y0) = (start.0 - REACH_RADIUS, start.1 - REACH_RADIUS);
        let mut reach = Reach { x0, y0, size, seen: vec![false; (size * size) as usize] };
        let mut queue = std::collections::VecDeque::new();
        if start_open {
            reach.mark(start.0, start.1);
            queue.push_back(start);
        }
        while let Some((x, y)) = queue.pop_front() {
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if reach.index(nx, ny).is_none() || reach.at(nx, ny) {
                    continue;
                }
                if step(x, y, dx, dy) {
                    reach.mark(nx, ny);
                    queue.push_back((nx, ny));
                }
            }
        }
        reach
    }

    fn index(&self, x: i64, y: i64) -> Option<usize> {
        let (lx, ly) = (x - self.x0, y - self.y0);
        (lx >= 0 && ly >= 0 && lx < self.size && ly < self.size).then(|| (ly * self.size + lx) as usize)
    }

    fn mark(&mut self, x: i64, y: i64) {
        if let Some(i) = self.index(x, y) {
            self.seen[i] = true;
        }
    }

    fn at(&self, x: i64, y: i64) -> bool {
        self.index(x, y).is_some_and(|i| self.seen[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eingang_zeigt_zum_bergfried() {
        assert_eq!(rotations_towards((0, 0), (10, 0))[0], 3);
        assert_eq!(rotations_towards((10, 0), (0, 0))[0], 1);
        assert_eq!(rotations_towards((0, 10), (0, 0))[0], 2);
        assert_eq!(rotations_towards((0, 0), (0, 10))[0], 0);
    }
}
