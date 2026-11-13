//! Navigationsgitter und Wegsuche.
//!
//! Zwei Nachbarfelder sind verbunden, wenn beide begehbar sind (kein Wasser)
//! und ihre gemeinsame Kante auf beiden Seiten gleich hoch ist. Damit sind
//! Rampen begehbar und Klippen nicht. Diagonal darf man nur gehen, wenn auch
//! beide geraden Umwege frei sind, so schneidet niemand Klippenecken ab.

use std::{cmp::Reverse, collections::BinaryHeap};

use protocol::TileKind;

/// Richtungen in Bit-Reihenfolge: O, SO, S, SW, W, NW, N, NO.
pub const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

pub struct NavGrid {
    pub width: u32,
    pub height: u32,
    /// Pro Feld ein Bit je Richtung aus [`DIRS`].
    pub dirs: Vec<u8>,
}

/// Eckhöhen der Kante eines Feldes in Richtung (dx, dy), von "links" nach "rechts"
/// so sortiert, dass die gegenüberliegende Kante des Nachbarn dieselbe Reihenfolge hat.
fn edge(c: &[u16; 4], dx: i32, dy: i32) -> [u16; 2] {
    // Ecken: 0 = NW, 1 = NO, 2 = SO, 3 = SW
    match (dx, dy) {
        (1, 0) => [c[1], c[2]],
        (-1, 0) => [c[0], c[3]],
        (0, 1) => [c[3], c[2]],
        _ => [c[0], c[1]],
    }
}

impl NavGrid {
    pub fn build(width: u32, height: u32, tiles: &[TileKind], corners: &[[u16; 4]]) -> NavGrid {
        let (w, h) = (width as i32, height as i32);
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h;
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        let walkable = |x: i32, y: i32| inside(x, y) && tiles[idx(x, y)] != TileKind::Water;
        let straight = |x: i32, y: i32, dx: i32, dy: i32| {
            walkable(x, y)
                && walkable(x + dx, y + dy)
                && edge(&corners[idx(x, y)], dx, dy) == edge(&corners[idx(x + dx, y + dy)], -dx, -dy)
        };

        let mut dirs = vec![0u8; (width * height) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut bits = 0u8;
                for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                    let ok = if dx == 0 || dy == 0 {
                        straight(x, y, dx, dy)
                    } else {
                        // Beide Wege um die Ecke müssen gehen.
                        straight(x, y, dx, 0)
                            && straight(x + dx, y, 0, dy)
                            && straight(x, y, 0, dy)
                            && straight(x, y + dy, dx, 0)
                    };
                    if ok {
                        bits |= 1 << b;
                    }
                }
                dirs[idx(x, y)] = bits;
            }
        }
        NavGrid { width, height, dirs }
    }

    fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.width as i32 + x) as usize
    }

    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.width as i32 && y < self.height as i32
    }

    /// Kann man von Feld (x, y) direkt zum Nachbarn (x + dx, y + dy) gehen?
    pub fn can_step(&self, x: i32, y: i32, dx: i32, dy: i32) -> bool {
        if !self.inside(x, y) {
            return false;
        }
        let Some(b) = DIRS.iter().position(|&d| d == (dx, dy)) else { return false };
        self.dirs[self.idx(x, y)] & (1 << b) != 0
    }

    /// Lässt sich das Feld überhaupt betreten?
    pub fn is_open(&self, x: i32, y: i32) -> bool {
        self.inside(x, y) && self.dirs[self.idx(x, y)] != 0
    }

    /// A*-Suche von Feld zu Feld. Ist das Ziel nicht erreichbar, führt der Weg
    /// zum erreichbaren Feld, das dem Ziel am nächsten ist.
    /// Liefert die Felder ohne das Startfeld.
    pub fn find_path(&self, start: (i32, i32), goal: (i32, i32)) -> Vec<(i32, i32)> {
        if !self.inside(start.0, start.1) {
            return Vec::new();
        }
        let n = (self.width * self.height) as usize;
        // Kosten: gerade 10, diagonal 14 (≈ 10 · √2).
        let heuristic = |(x, y): (i32, i32)| {
            let (dx, dy) = ((x - goal.0).abs(), (y - goal.1).abs());
            10 * dx.max(dy) + 4 * dx.min(dy)
        };
        let mut cost = vec![u32::MAX; n];
        let mut came_from = vec![usize::MAX; n];
        let mut open = BinaryHeap::new();
        let s = self.idx(start.0, start.1);
        cost[s] = 0;
        open.push(Reverse((heuristic(start) as u32, s)));
        let mut best = (heuristic(start), s);

        while let Some(Reverse((_, i))) = open.pop() {
            let (x, y) = ((i as u32 % self.width) as i32, (i as u32 / self.width) as i32);
            if (x, y) == goal {
                best = (0, i);
                break;
            }
            let h = heuristic((x, y));
            if h < best.0 {
                best = (h, i);
            }
            for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                if self.dirs[i] & (1 << b) == 0 {
                    continue;
                }
                let j = self.idx(x + dx, y + dy);
                let c = cost[i] + if dx != 0 && dy != 0 { 14 } else { 10 };
                if c < cost[j] {
                    cost[j] = c;
                    came_from[j] = i;
                    open.push(Reverse((c + heuristic((x + dx, y + dy)) as u32, j)));
                }
            }
        }

        let mut path = Vec::new();
        let mut i = best.1;
        while i != s {
            path.push(((i as u32 % self.width) as i32, (i as u32 / self.width) as i32));
            i = came_from[i];
        }
        path.reverse();
        path
    }

    /// Kommt man auf gerader Linie von a nach b, ohne Klippe oder Wasser zu kreuzen?
    pub fn line_walkable(&self, a: (f32, f32), b: (f32, f32)) -> bool {
        let dist = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let steps = (dist / 0.1).ceil().max(1.0) as i32;
        let mut cur = (a.0.floor() as i32, a.1.floor() as i32);
        for k in 1..=steps {
            let t = k as f32 / steps as f32;
            let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            let next = (p.0.floor() as i32, p.1.floor() as i32);
            if next != cur {
                let (dx, dy) = (next.0 - cur.0, next.1 - cur.1);
                if dx.abs() > 1 || dy.abs() > 1 || !self.can_step(cur.0, cur.1, dx, dy) {
                    return false;
                }
                cur = next;
            }
        }
        true
    }

    /// Weg als Wegpunkte in Feld-Koordinaten. Unnötige Zwischenpunkte werden
    /// weggelassen, solange die gerade Linie begehbar bleibt.
    pub fn waypoints(&self, from: (f32, f32), to: (f32, f32)) -> Vec<(f32, f32)> {
        let start = (from.0.floor() as i32, from.1.floor() as i32);
        let goal = (to.0.floor() as i32, to.1.floor() as i32);
        let cells = self.find_path(start, goal);
        let reached_goal = cells.last().copied().unwrap_or(start) == goal;

        let mut points: Vec<(f32, f32)> =
            cells.iter().map(|&(x, y)| (x as f32 + 0.5, y as f32 + 0.5)).collect();
        if reached_goal && self.is_open(goal.0, goal.1) {
            // Genau auf den geklickten Punkt im Zielfeld laufen.
            points.pop();
            points.push(to);
        }

        // Wegpunkte glätten: von jedem Punkt so weit springen wie möglich.
        let mut result = Vec::new();
        let mut cur = from;
        let mut i = 0;
        while i < points.len() {
            let mut j = points.len() - 1;
            while j > i && !self.line_walkable(cur, points[j]) {
                j -= 1;
            }
            result.push(points[j]);
            cur = points[j];
            i = j + 1;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use crate::map::{GameMap, DEFAULT_MAP};

    #[test]
    fn klippen_blockieren_rampen_nicht() {
        // Links Tal, rechts Plateau; nur die mittlere Zeile hat eine Rampe.
        let map = GameMap::parse("0001\n0>>1\n0001\n").unwrap();
        let nav = &map.nav;
        assert!(!nav.can_step(2, 0, 1, 0), "Klippe zwischen Stufe 0 und 1");
        assert!(nav.can_step(0, 1, 1, 0), "Tal auf Rampe");
        assert!(nav.can_step(2, 1, 1, 0), "Rampe auf Plateau");
        assert!(!nav.can_step(1, 1, 0, 1), "Seitlich von der Rampe ist eine Kante");
        let path = nav.find_path((0, 0), (3, 0));
        assert_eq!(path.last(), Some(&(3, 0)));
        assert!(path.contains(&(1, 1)) && path.contains(&(2, 1)), "Weg führt über die Rampe");
    }

    #[test]
    fn wasser_ist_nicht_begehbar() {
        let map = GameMap::parse("0~0\n0~0\n0~0\n").unwrap();
        let path = map.nav.find_path((0, 1), (2, 1));
        assert_ne!(path.last(), Some(&(2, 1)));
        assert!(map.nav.waypoints((0.5, 1.5), (2.5, 1.5)).iter().all(|p| p.0 < 1.0));
    }

    #[test]
    fn beide_burgen_sind_verbunden() {
        let map = GameMap::parse(DEFAULT_MAP).unwrap();
        let (a, b) = (map.spawns[0], map.spawns[1]);
        let path = map.nav.find_path((a.0 as i32, a.1 as i32), (b.0 as i32, b.1 as i32));
        assert_eq!(path.last(), Some(&(b.0 as i32, b.1 as i32)));
    }

    #[test]
    fn alles_land_ist_erreichbar() {
        let map = GameMap::parse(DEFAULT_MAP).unwrap();
        let s = map.spawns[0];
        for y in 0..map.height as i32 {
            for x in 0..map.width as i32 {
                if map.tiles[(y as u32 * map.width + x as u32) as usize] != protocol::TileKind::Water {
                    continue;
                }
                assert!(!map.nav.is_open(x, y));
            }
        }
        // Jedes Feld, das nicht Wasser ist, soll vom ersten Spawn erreichbar sein.
        for y in 0..map.height as i32 {
            for x in 0..map.width as i32 {
                if map.nav.is_open(x, y) {
                    let p = map.nav.find_path((s.0 as i32, s.1 as i32), (x, y));
                    assert!(
                        (x, y) == (s.0 as i32, s.1 as i32) || p.last() == Some(&(x, y)),
                        "Feld ({x}, {y}) ist nicht erreichbar"
                    );
                }
            }
        }
    }
}
