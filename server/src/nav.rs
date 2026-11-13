//! Navigationsgitter und Wegsuche.
//!
//! Zwei Nachbarfelder sind verbunden, wenn beide begehbar sind (kein Wasser)
//! und sich ihre Höhenstufen um höchstens eins unterscheiden (dann liegt
//! dazwischen eine Schräge, sonst eine Wand). Diagonal darf man nur gehen,
//! wenn auch beide geraden Umwege frei sind, so schneidet niemand Ecken ab.
//!
//! Gebäude und Wald blockieren Felder zusätzlich. Das Gelände ändert sich nie,
//! deshalb merken wir uns seine Verbindungen getrennt und rechnen bei einer
//! Blockade nur die Felder drumherum neu.
//!
//! Wegsuche auf großen Karten (bis 1024 × 1024) läuft hierarchisch: Die Karte
//! ist in Blöcke zu 16 × 16 Feldern geteilt, jeder Block in zusammenhängende
//! Gebiete. Erst wird über die Gebiete grob gesucht, dann nur innerhalb dieser
//! Gebiete feldgenau.

use std::{cell::RefCell, cmp::Reverse, collections::BinaryHeap};

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

/// Kantenlänge eines Blocks in Feldern.
const CLUSTER: i32 = 16;
/// Kein Gebiet (Feld nicht begehbar).
const NO_REGION: u32 = u32::MAX;
/// Wie weit das Glätten höchstens vorausschaut (in Wegfeldern).
const SMOOTH_LOOKAHEAD: usize = 48;

#[derive(Clone, Default)]
struct Region {
    /// Feld nahe der Mitte des Gebiets (gehört selbst dazu).
    center: (i32, i32),
    /// Benachbarte Gebiete (globale Ids).
    neighbors: Vec<u32>,
}

/// Wiederverwendbarer Speicher für die Suchen, damit nicht jede Suche
/// Millionen Einträge anlegen muss. Ein Eintrag gilt nur, wenn sein Stempel
/// zur aktuellen Suche passt.
#[derive(Default)]
struct Scratch {
    stamp: u32,
    seen: Vec<u32>,
    cost: Vec<u32>,
    came: Vec<u32>,
    allowed: Vec<u32>,
    rseen: Vec<u32>,
    rcost: Vec<u32>,
    rcame: Vec<u32>,
}

pub struct NavGrid {
    pub width: u32,
    pub height: u32,
    /// Pro Feld ein Bit je Richtung aus [`DIRS`], mit Blockaden.
    pub dirs: Vec<u8>,
    /// Nur das Gelände, ohne Blockaden.
    terrain: Vec<u8>,
    /// Wie viele Dinge dieses Feld blockieren (Gebäude, Wald, ...).
    blocked: Vec<u8>,
    /// Gebiet je Feld: Block · 256 + Nummer im Block.
    region: Vec<u32>,
    /// Gebiete je Block.
    clusters: Vec<Vec<Region>>,
    cw: i32,
    ch: i32,
    scratch: RefCell<Scratch>,
}

impl NavGrid {
    /// `levels`: Höhenstufe je Feld, `water`: Wasserfelder, `blocked`: Felder,
    /// die von Anfang an blockiert sind (Wald, Felsen).
    pub fn build(width: u32, height: u32, levels: &[u8], water: &[bool], blocked: &[bool]) -> NavGrid {
        let (w, h) = (width as i32, height as i32);
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h;
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        let straight = |x: i32, y: i32, dx: i32, dy: i32| {
            let (nx, ny) = (x + dx, y + dy);
            inside(x, y)
                && inside(nx, ny)
                && !water[idx(x, y)]
                && !water[idx(nx, ny)]
                && levels[idx(x, y)].abs_diff(levels[idx(nx, ny)]) <= 1
        };

        let n = (width * height) as usize;
        let mut terrain = vec![0u8; n];
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
                terrain[idx(x, y)] = bits;
            }
        }
        let (cw, ch) = ((w + CLUSTER - 1) / CLUSTER, (h + CLUSTER - 1) / CLUSTER);
        // Anfangs-Blockaden über die Blockiert-Ebene, damit sie später per
        // `set_blocked(.., false)` wieder freigegeben werden können.
        let mut nav = NavGrid {
            width,
            height,
            dirs: terrain.clone(),
            terrain,
            blocked: blocked.iter().map(|&b| b as u8).collect(),
            region: vec![NO_REGION; n],
            clusters: vec![Vec::new(); (cw * ch) as usize],
            cw,
            ch,
            scratch: RefCell::new(Scratch::default()),
        };
        if blocked.iter().any(|&b| b) {
            for y in 0..h {
                for x in 0..w {
                    nav.recompute(x, y);
                }
            }
        }
        for c in 0..cw * ch {
            nav.label_cluster(c);
        }
        for c in 0..cw * ch {
            nav.link_cluster(c);
        }
        nav
    }

    /// Feld blockieren (`true`) oder wieder freigeben. Mehrfaches Blockieren
    /// wird gezählt, frei ist das Feld erst, wenn alle Blockaden weg sind.
    pub fn set_blocked(&mut self, x: i32, y: i32, on: bool) {
        if !self.inside(x, y) {
            return;
        }
        let i = self.idx(x, y);
        let before = self.blocked[i] > 0;
        self.blocked[i] = if on { self.blocked[i].saturating_add(1) } else { self.blocked[i].saturating_sub(1) };
        if before == (self.blocked[i] > 0) {
            return; // An den Verbindungen ändert sich nichts.
        }
        for ny in y - 1..=y + 1 {
            for nx in x - 1..=x + 1 {
                if self.inside(nx, ny) {
                    self.recompute(nx, ny);
                }
            }
        }
        // Betroffene Blöcke neu einteilen, danach deren Nachbarn neu verknüpfen.
        let (cx0, cy0) = (((x - 1).max(0)) / CLUSTER, ((y - 1).max(0)) / CLUSTER);
        let (cx1, cy1) = ((x + 1).min(self.width as i32 - 1) / CLUSTER, (y + 1).min(self.height as i32 - 1) / CLUSTER);
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                self.label_cluster(cy * self.cw + cx);
            }
        }
        for cy in (cy0 - 1).max(0)..=(cy1 + 1).min(self.ch - 1) {
            for cx in (cx0 - 1).max(0)..=(cx1 + 1).min(self.cw - 1) {
                self.link_cluster(cy * self.cw + cx);
            }
        }
    }

    fn recompute(&mut self, x: i32, y: i32) {
        let i = self.idx(x, y);
        let free = |nx: i32, ny: i32| self.inside(nx, ny) && self.blocked[self.idx(nx, ny)] == 0;
        let mut bits = 0u8;
        if free(x, y) {
            for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                // Diagonal müssen auch beide Felder um die Ecke frei sein.
                if self.terrain[i] & (1 << b) != 0
                    && free(x + dx, y + dy)
                    && free(x + dx, y)
                    && free(x, y + dy)
                {
                    bits |= 1 << b;
                }
            }
        }
        self.dirs[i] = bits;
    }

    fn cluster_rect(&self, c: i32) -> (i32, i32, i32, i32) {
        let (x0, y0) = ((c % self.cw) * CLUSTER, (c / self.cw) * CLUSTER);
        (x0, y0, (x0 + CLUSTER).min(self.width as i32), (y0 + CLUSTER).min(self.height as i32))
    }

    /// Zusammenhängende Gebiete innerhalb eines Blocks bestimmen.
    fn label_cluster(&mut self, c: i32) {
        let (x0, y0, x1, y1) = self.cluster_rect(c);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = self.idx(x, y);
                self.region[i] = NO_REGION;
            }
        }
        let mut regions = Vec::new();
        let mut stack = Vec::new();
        let mut members = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let i = self.idx(x, y);
                if self.region[i] != NO_REGION || self.dirs[i] == 0 {
                    continue;
                }
                let id = c as u32 * 256 + regions.len() as u32;
                self.region[i] = id;
                stack.push((x, y));
                members.clear();
                let (mut sx, mut sy) = (0i64, 0i64);
                while let Some((px, py)) = stack.pop() {
                    members.push((px, py));
                    sx += px as i64;
                    sy += py as i64;
                    let pi = self.idx(px, py);
                    for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                        let (nx, ny) = (px + dx, py + dy);
                        if self.dirs[pi] & (1 << b) == 0 || nx < x0 || ny < y0 || nx >= x1 || ny >= y1 {
                            continue;
                        }
                        let ni = self.idx(nx, ny);
                        if self.region[ni] == NO_REGION {
                            self.region[ni] = id;
                            stack.push((nx, ny));
                        }
                    }
                }
                // Mittelpunkt: das Feld des Gebiets, das dem Schwerpunkt am nächsten liegt.
                let k = members.len() as i64;
                let (mx, my) = (sx / k, sy / k);
                let center = *members
                    .iter()
                    .min_by_key(|&&(px, py)| (px as i64 - mx).pow(2) + (py as i64 - my).pow(2))
                    .unwrap();
                regions.push(Region { center, neighbors: Vec::new() });
            }
        }
        self.clusters[c as usize] = regions;
    }

    /// Verbindungen der Gebiete eines Blocks zu Gebieten in Nachbarblöcken.
    fn link_cluster(&mut self, c: i32) {
        let (x0, y0, x1, y1) = self.cluster_rect(c);
        let mut links: Vec<Vec<u32>> = vec![Vec::new(); self.clusters[c as usize].len()];
        for y in y0..y1 {
            for x in x0..x1 {
                if x != x0 && y != y0 && x != x1 - 1 && y != y1 - 1 {
                    continue; // nur der Rand kann in andere Blöcke führen
                }
                let i = self.idx(x, y);
                let r = self.region[i];
                if r == NO_REGION {
                    continue;
                }
                for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                    if self.dirs[i] & (1 << b) == 0 {
                        continue;
                    }
                    let other = self.region[self.idx(x + dx, y + dy)];
                    let list = &mut links[(r % 256) as usize];
                    if other / 256 != r / 256 && !list.contains(&other) {
                        list.push(other);
                    }
                }
            }
        }
        for (region, list) in self.clusters[c as usize].iter_mut().zip(links) {
            region.neighbors = list;
        }
    }

    fn region_info(&self, r: u32) -> &Region {
        &self.clusters[(r / 256) as usize][(r % 256) as usize]
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

    /// Grobe Suche über die Gebiete. Liefert die Gebiete, durch die der Weg
    /// führt (Start bis Ziel bzw. bis zum Gebiet, das dem Ziel am nächsten liegt).
    fn region_path(&self, s: &mut Scratch, from: u32, goal_cell: (i32, i32), goal: u32) -> Vec<u32> {
        let octile = |a: (i32, i32), b: (i32, i32)| {
            let (dx, dy) = ((a.0 - b.0).unsigned_abs(), (a.1 - b.1).unsigned_abs());
            10 * dx.max(dy) + 4 * dx.min(dy)
        };
        let stamp = s.stamp;
        let goal_center = if goal == NO_REGION { goal_cell } else { self.region_info(goal).center };
        let mut open = BinaryHeap::new();
        s.rseen[from as usize] = stamp;
        s.rcost[from as usize] = 0;
        s.rcame[from as usize] = NO_REGION;
        open.push(Reverse((octile(self.region_info(from).center, goal_center), from)));
        let mut best = (octile(self.region_info(from).center, goal_cell), from);
        while let Some(Reverse((_, r))) = open.pop() {
            if r == goal {
                best = (0, r);
                break;
            }
            let info = self.region_info(r);
            let h = octile(info.center, goal_cell);
            if h < best.0 {
                best = (h, r);
            }
            for &nb in &info.neighbors {
                let c = s.rcost[r as usize] + octile(info.center, self.region_info(nb).center);
                let ni = nb as usize;
                if s.rseen[ni] != stamp || c < s.rcost[ni] {
                    s.rseen[ni] = stamp;
                    s.rcost[ni] = c;
                    s.rcame[ni] = r;
                    open.push(Reverse((c + octile(self.region_info(nb).center, goal_center), nb)));
                }
            }
        }
        let mut path = vec![best.1];
        while let Some(&r) = path.last() {
            let prev = s.rcame[r as usize];
            if prev == NO_REGION {
                break;
            }
            path.push(prev);
        }
        path
    }

    /// Wegsuche von Feld zu Feld. Ist das Ziel nicht erreichbar, führt der Weg
    /// zum erreichbaren Feld, das dem Ziel am nächsten ist.
    /// Liefert die Felder ohne das Startfeld.
    pub fn find_path(&self, start: (i32, i32), goal: (i32, i32)) -> Vec<(i32, i32)> {
        if !self.inside(start.0, start.1) {
            return Vec::new();
        }
        let from = self.region[self.idx(start.0, start.1)];
        if from == NO_REGION {
            return Vec::new(); // eingemauert
        }
        let goal_region =
            if self.inside(goal.0, goal.1) { self.region[self.idx(goal.0, goal.1)] } else { NO_REGION };

        let mut guard = self.scratch.borrow_mut();
        let s = &mut *guard;
        let n = (self.width * self.height) as usize;
        if s.seen.len() != n {
            s.seen = vec![0; n];
            s.cost = vec![0; n];
            s.came = vec![0; n];
            let r = self.clusters.len() * 256;
            s.allowed = vec![0; r];
            s.rseen = vec![0; r];
            s.rcost = vec![0; r];
            s.rcame = vec![0; r];
        }
        s.stamp = s.stamp.wrapping_add(1);
        if s.stamp == 0 {
            // Überlauf: alles zurücksetzen, sonst gelten alte Einträge.
            for v in [&mut s.seen, &mut s.allowed, &mut s.rseen] {
                v.iter_mut().for_each(|e| *e = 0);
            }
            s.stamp = 1;
        }
        let stamp = s.stamp;

        // Grob über die Gebiete, dann feldgenau nur durch diese Gebiete.
        let corridor = self.region_path(s, from, goal, goal_region);
        for &r in &corridor {
            s.allowed[r as usize] = stamp;
        }
        // Ist das Ziel unerreichbar, endet der Korridor im nächstgelegenen Gebiet
        // und die Suche liefert dort das Feld, das dem Ziel am nächsten ist.
        let target = goal;

        let heuristic = |(x, y): (i32, i32)| {
            let (dx, dy) = ((x - target.0).unsigned_abs(), (y - target.1).unsigned_abs());
            10 * dx.max(dy) + 4 * dx.min(dy)
        };
        let mut open = BinaryHeap::new();
        let si = self.idx(start.0, start.1);
        s.seen[si] = stamp;
        s.cost[si] = 0;
        open.push(Reverse((heuristic(start), si as u32)));
        let mut best = (heuristic(start), si);
        while let Some(Reverse((f, i))) = open.pop() {
            let i = i as usize;
            let (x, y) = ((i as u32 % self.width) as i32, (i as u32 / self.width) as i32);
            let ci = s.cost[i];
            let h = heuristic((x, y));
            if f > ci + h {
                continue; // veralteter Eintrag
            }
            if (x, y) == target {
                best = (0, i);
                break;
            }
            if h < best.0 {
                best = (h, i);
            }
            for (b, &(dx, dy)) in DIRS.iter().enumerate() {
                if self.dirs[i] & (1 << b) == 0 {
                    continue;
                }
                let j = self.idx(x + dx, y + dy);
                if s.allowed[self.region[j] as usize] != stamp {
                    continue;
                }
                let c = ci + if dx != 0 && dy != 0 { 14 } else { 10 };
                if s.seen[j] != stamp || c < s.cost[j] {
                    s.seen[j] = stamp;
                    s.cost[j] = c;
                    s.came[j] = i as u32;
                    open.push(Reverse((c + heuristic((x + dx, y + dy)), j as u32)));
                }
            }
        }

        let mut path = Vec::new();
        let mut i = best.1;
        while i != si {
            path.push(((i as u32 % self.width) as i32, (i as u32 / self.width) as i32));
            i = s.came[i] as usize;
        }
        path.reverse();
        path
    }

    /// Kommt man auf gerader Linie von a nach b, ohne Wand, Wasser oder
    /// Blockade zu kreuzen? Geht Feld für Feld entlang der Linie.
    pub fn line_walkable(&self, a: (f32, f32), b: (f32, f32)) -> bool {
        let (mut cx, mut cy) = (a.0.floor() as i32, a.1.floor() as i32);
        let (ex, ey) = (b.0.floor() as i32, b.1.floor() as i32);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (sx, sy) = (dx.signum() as i32, dy.signum() as i32);
        let next_x = if sx > 0 { cx as f32 + 1.0 } else { cx as f32 };
        let next_y = if sy > 0 { cy as f32 + 1.0 } else { cy as f32 };
        let mut tx = if sx != 0 { (next_x - a.0) / dx } else { f32::INFINITY };
        let mut ty = if sy != 0 { (next_y - a.1) / dy } else { f32::INFINITY };
        let ddx = if sx != 0 { 1.0 / dx.abs() } else { f32::INFINITY };
        let ddy = if sy != 0 { 1.0 / dy.abs() } else { f32::INFINITY };
        let limit = (ex - cx).abs() + (ey - cy).abs() + 2;
        for _ in 0..limit {
            if (cx, cy) == (ex, ey) {
                return true;
            }
            const EPS: f32 = 1e-5;
            let (stx, sty) = if (tx - ty).abs() < EPS {
                (sx, sy) // genau durch eine Ecke: diagonaler Schritt
            } else if tx < ty {
                (sx, 0)
            } else {
                (0, sy)
            };
            if !self.can_step(cx, cy, stx, sty) {
                return false;
            }
            if stx != 0 {
                tx += ddx;
            }
            if sty != 0 {
                ty += ddy;
            }
            cx += stx;
            cy += sty;
        }
        (cx, cy) == (ex, ey)
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

        // Wegpunkte glätten: von jedem Punkt so weit springen wie möglich
        // (begrenzt, damit lange Wege nicht quadratisch teuer werden).
        let mut result = Vec::new();
        let mut cur = from;
        let mut i = 0;
        while i < points.len() {
            let mut j = (i + SMOOTH_LOOKAHEAD).min(points.len() - 1);
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
    fn schraegen_ja_waende_nein() {
        // Links Stufe 0, rechts Stufe 2; nur in der mittleren Zeile führt eine
        // Stufe 1 dazwischen hindurch.
        let map = GameMap::parse("0022\n0122\n0022\n").unwrap();
        let nav = &map.nav;
        assert!(!nav.can_step(1, 0, 1, 0), "Wand zwischen Stufe 0 und 2");
        assert!(nav.can_step(0, 1, 1, 0), "0 auf 1");
        assert!(nav.can_step(1, 1, 1, 0), "1 auf 2");
        let path = nav.find_path((0, 0), (3, 0));
        assert_eq!(path.last(), Some(&(3, 0)));
        assert!(path.contains(&(1, 1)), "Weg führt über die Schräge");
    }

    #[test]
    fn wasser_ist_nicht_begehbar() {
        let map = GameMap::parse("0~0\n0~0\n0~0\n").unwrap();
        let path = map.nav.find_path((0, 1), (2, 1));
        assert_ne!(path.last(), Some(&(2, 1)));
        assert!(map.nav.waypoints((0.5, 1.5), (2.5, 1.5)).iter().all(|p| p.0 < 1.0));
    }

    #[test]
    fn gebaeude_blockieren_und_geben_frei() {
        let mut map = GameMap::parse("000\n000\n000\n").unwrap();
        let nav = &mut map.nav;
        // Mittlere Spalte zumauern: kein Weg mehr von links nach rechts.
        for y in 0..3 {
            nav.set_blocked(1, y, true);
        }
        assert!(!nav.is_open(1, 1));
        assert!(!nav.can_step(0, 0, 1, 1), "nicht diagonal an der Mauer vorbei");
        assert_ne!(nav.find_path((0, 1), (2, 1)).last(), Some(&(2, 1)));
        nav.set_blocked(1, 2, false);
        assert_eq!(nav.find_path((0, 0), (2, 0)).last(), Some(&(2, 0)), "Lücke unten");
        assert!(nav.can_step(0, 2, 1, 0));
        assert!(!nav.can_step(0, 1, 1, 1), "Ecke der Mauer nicht abschneiden");
    }

    /// Text einer flachen Karte mit einer Mauer quer durch, die nur unten ein Loch hat.
    fn grosse_karte(size: usize) -> String {
        let mut s = String::new();
        for y in 0..size {
            for x in 0..size {
                s.push(if x == size / 2 && y < size - 5 { '~' } else { '0' });
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn hierarchische_suche_findet_umweg() {
        let map = GameMap::parse(&grosse_karte(200)).unwrap();
        let path = map.nav.find_path((10, 10), (190, 10));
        assert_eq!(path.last(), Some(&(190, 10)));
        assert!(path.iter().any(|&(_, y)| y >= 195), "Weg geht durch das Loch unten");
        // Jeder Schritt ist ein erlaubter Nachbarschritt.
        let mut cur = (10, 10);
        for &p in &path {
            assert!(map.nav.can_step(cur.0, cur.1, p.0 - cur.0, p.1 - cur.1));
            cur = p;
        }
    }

    #[test]
    fn blockade_teilt_gebiete_neu() {
        let mut map = GameMap::parse(&grosse_karte(64)).unwrap();
        // Das Loch unten schließen: jetzt geht es nur noch bis an die Mauer.
        for y in 59..64 {
            map.nav.set_blocked(32, y, true);
        }
        let path = map.nav.find_path((10, 10), (50, 10));
        assert_ne!(path.last(), Some(&(50, 10)));
        assert!(path.last().unwrap().0 < 32);
        map.nav.set_blocked(32, 63, false);
        assert_eq!(map.nav.find_path((10, 10), (50, 10)).last(), Some(&(50, 10)));
    }

    #[test]
    fn gerade_linie_prueft_jedes_feld() {
        let map = GameMap::parse("000\n0~0\n000\n").unwrap();
        assert!(!map.nav.line_walkable((0.5, 0.5), (2.5, 2.5)), "durchs Wasser");
        assert!(map.nav.line_walkable((0.5, 0.5), (2.5, 0.5)));
        assert!(!map.nav.line_walkable((0.2, 0.5), (2.5, 1.2)), "streift das Wasser");
    }

    #[test]
    fn beide_burgen_sind_verbunden() {
        let map = GameMap::parse(DEFAULT_MAP).unwrap();
        let (a, b) = (map.spawns[0], map.spawns[1]);
        let path = map.nav.find_path((a.0 as i32, a.1 as i32), (b.0 as i32, b.1 as i32));
        assert_eq!(path.last(), Some(&(b.0 as i32, b.1 as i32)));
    }
}
