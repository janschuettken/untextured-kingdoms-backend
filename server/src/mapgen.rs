//! Zufallskarten beliebiger Größe (bis 1024 × 1024) für bis zu 8 Spieler.
//!
//! Ablauf: Höhen aus Rauschen mit Terrassen (mal sanfte Schrägen, mal Wände),
//! Seen in den Senken, ebene Startplätze im Kreis, danach werden abgeschnittene
//! Gebiete mit Rampen (bzw. Furten) angebunden. Zuletzt Ressourcen: Wald in
//! Gruppen, Stein- und Eisenvorkommen ab 5 × 5 Feldern, Äcker ab 7 × 7.
//! Flüsse laufen von Rand zu Rand, mit Furten und einigen neutralen Brücken.
//! Mengen, Hügel und Flüsse stellt [`Params`] ein.
//!
//! Gleicher Seed, gleiche Karte.

use std::fmt::Write;

use crate::map::MAX_LEVEL;

pub struct Params {
    pub width: usize,
    pub height: usize,
    pub players: usize,
    pub seed: u64,
    /// Mengen als Faktor, 1,0 = normal, 0 = keine, 2,0 = doppelt.
    pub farmland: f32,
    pub forest: f32,
    pub stone: f32,
    pub iron: f32,
    /// Größe der einzelnen Wald-, Stein- und Eisenflecken als Faktor auf den
    /// Radius (1,0 = normal, 0,5 bis 3,0). Die Menge oben bestimmt die Anzahl.
    pub forest_size: f32,
    pub stone_size: f32,
    pub iron_size: f32,
    /// 0 = flach, 1 = normal, 2 = sehr hügelig.
    pub hills: f32,
    /// Anzahl Flüsse.
    pub rivers: usize,
}

impl Params {
    pub fn new(width: usize, height: usize, players: usize, seed: u64) -> Self {
        Params {
            width,
            height,
            players,
            seed,
            farmland: 1.0,
            forest: 1.0,
            stone: 1.0,
            iron: 1.0,
            forest_size: 1.0,
            stone_size: 1.0,
            iron_size: 1.0,
            hills: 1.0,
            rivers: 1,
        }
    }
}

/// Wie oft etwas bei Faktor `f` passiert, wenn es bei 1,0 `base`-mal passiert:
/// der Bruchteil entscheidet der Zufall.
fn times(rng: &mut Rng, base: f32, f: f32) -> usize {
    let x = base * f;
    x.floor() as usize + usize::from(rng.float() < x.fract())
}

/// Vorkommen werden erst über 100 % größer, darunter nur seltener (sie sollen
/// nicht unter die Mindestgröße fallen).
fn grow(r: f32, f: f32) -> f32 {
    r * (1.0 + 0.35 * (f - 1.0).max(0.0))
}

/// Kleinster Radius eines Stein- oder Eisenflecks: so bleibt er mindestens fünf Felder breit.
const MIN_DEPOSIT_RADIUS: f32 = 2.0;
/// Größenregler: von halber bis dreifacher Fleckgröße.
pub const SIZE_RANGE: (f32, f32) = (0.5, 3.0);

/// SplitMix64: klein, schnell und für Karten zufällig genug.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn float(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.float()
    }
}

/// Wertrauschen: Zufallswerte auf einem Gitter, weich interpoliert.
struct Noise {
    seed: u64,
}

impl Noise {
    fn lattice(&self, x: i32, y: i32) -> f32 {
        let mut h = self.seed ^ (x as u64).wrapping_mul(0x8DA6_B343) ^ (y as u64).wrapping_mul(0xD816_3841);
        h = (h ^ (h >> 33)).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h = (h ^ (h >> 33)).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        ((h ^ (h >> 33)) >> 40) as f32 / (1u64 << 24) as f32
    }
    fn value(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let (ix, iy) = (x0 as i32, y0 as i32);
        let a = self.lattice(ix, iy);
        let b = self.lattice(ix + 1, iy);
        let c = self.lattice(ix, iy + 1);
        let d = self.lattice(ix + 1, iy + 1);
        let top = a + (b - a) * sx;
        let bottom = c + (d - c) * sx;
        top + (bottom - top) * sy
    }
    /// Mehrere Oktaven, Ergebnis ungefähr 0..1.
    fn fbm(&self, x: f32, y: f32, wavelength: f32, octaves: u32) -> f32 {
        let (mut sum, mut amp, mut norm, mut f) = (0.0, 1.0, 0.0, 1.0 / wavelength);
        for o in 0..octaves {
            // Jede Oktave etwas versetzt, damit sich keine Gitterlinien stapeln.
            let off = o as f32 * 17.31;
            sum += amp * self.value(x * f + off, y * f - off);
            norm += amp;
            amp *= 0.5;
            f *= 2.0;
        }
        sum / norm
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

const WATER: i8 = -1;
pub const MAX_RIVERS: usize = 8;

struct Gen {
    w: usize,
    h: usize,
    /// Höhenstufe je Feld, [`WATER`] für Wasser.
    level: Vec<i8>,
    res: Vec<u8>,
    /// Hier keine Ressourcen (Startplätze).
    reserved: Vec<bool>,
}

impl Gen {
    /// Darf man von Feld i zum Nachbarn j gehen (nur Gelände)?
    fn step_ok(&self, i: usize, j: usize) -> bool {
        self.level[i] != WATER && self.level[j] != WATER && (self.level[i] - self.level[j]).abs() <= 1
    }

    /// Gebiete, die nur der Wald abschneidet, mit einer Schneise anbinden;
    /// winzige Taschen einfach zuwachsen lassen. So meldet die Prüfung nichts.
    fn open_forest_pockets(&mut self, from: (i32, i32)) {
        let n = self.w * self.h;
        let blocks = |c: u8| matches!(c, b'W' | b'S' | b'E');
        for _round in 0..400 {
            // Erreichbar vom ersten Startplatz (mit Wald als Hindernis).
            let mut reach = vec![false; n];
            let start = self.idx(from.0, from.1);
            let mut stack = vec![start];
            reach[start] = true;
            while let Some(i) = stack.pop() {
                for j in self.neighbors4(i) {
                    if !reach[j] && !blocks(self.res[j]) && self.step_ok(i, j) {
                        reach[j] = true;
                        stack.push(j);
                    }
                }
            }
            let Some(pocket) = (0..n).find(|&i| !reach[i] && self.level[i] != WATER && !blocks(self.res[i])) else {
                return;
            };
            // Größe der Tasche.
            let mut cells = vec![pocket];
            let mut seen = vec![false; n];
            seen[pocket] = true;
            let mut k = 0;
            while k < cells.len() {
                let i = cells[k];
                k += 1;
                for j in self.neighbors4(i) {
                    if !seen[j] && !blocks(self.res[j]) && self.step_ok(i, j) {
                        seen[j] = true;
                        cells.push(j);
                    }
                }
            }
            if cells.len() < 30 {
                for i in cells {
                    self.res[i] = b'W';
                }
                continue;
            }
            // Kürzester Weg durch den Wald zu erreichbarem Land, dann eine
            // zwei Felder breite Schneise schlagen. Erst nur durch Wald; liegt
            // ein Vorkommen im Weg (etwa auf einer Furt), notfalls auch hindurch.
            let mut found = None;
            for through_deposits in [false, true] {
                let mut came = vec![usize::MAX; n];
                let mut queue = std::collections::VecDeque::from([pocket]);
                came[pocket] = pocket;
                while let Some(i) = queue.pop_front() {
                    if reach[i] {
                        found = Some((i, came));
                        break;
                    }
                    for j in self.neighbors4(i) {
                        let free = through_deposits || (self.res[j] != b'S' && self.res[j] != b'E');
                        if came[j] == usize::MAX && free && self.step_ok(i, j) {
                            came[j] = i;
                            queue.push_back(j);
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            let Some((mut i, came)) = found else {
                for c in cells {
                    if !self.reserved[c] {
                        self.res[c] = b'W';
                    }
                }
                continue;
            };
            while i != pocket {
                for j in std::iter::once(i).chain(self.neighbors4(i).into_iter().take(2)) {
                    if blocks(self.res[j]) {
                        self.res[j] = b'.';
                    }
                }
                i = came[i];
            }
        }
    }

    fn neighbors4(&self, i: usize) -> Vec<usize> {
        let (x, y) = ((i % self.w) as i32, (i / self.w) as i32);
        [(1, 0), (0, 1), (-1, 0), (0, -1)]
            .into_iter()
            .map(|(dx, dy)| (x + dx, y + dy))
            .filter(|&(nx, ny)| self.inside(nx, ny))
            .map(|(nx, ny)| self.idx(nx, ny))
            .collect()
    }

    fn idx(&self, x: i32, y: i32) -> usize {
        y as usize * self.w + x as usize
    }
    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h
    }

    /// Zusammenhängende Gebiete nach Gelände (Schritt nur bei höchstens einer
    /// Stufe Unterschied). Liefert Gebietsnummer je Feld (u32::MAX = Wasser).
    fn regions(&self) -> (Vec<u32>, Vec<usize>) {
        let n = self.w * self.h;
        let mut region = vec![u32::MAX; n];
        let mut sizes = Vec::new();
        let mut stack = Vec::new();
        for start in 0..n {
            if region[start] != u32::MAX || self.level[start] == WATER {
                continue;
            }
            let id = sizes.len() as u32;
            region[start] = id;
            stack.push(start);
            let mut size = 0;
            while let Some(i) = stack.pop() {
                size += 1;
                let (x, y) = ((i % self.w) as i32, (i / self.w) as i32);
                for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let (nx, ny) = (x + dx, y + dy);
                    if !self.inside(nx, ny) {
                        continue;
                    }
                    let j = self.idx(nx, ny);
                    if region[j] == u32::MAX && self.level[j] != WATER && (self.level[j] - self.level[i]).abs() <= 1 {
                        region[j] = id;
                        stack.push(j);
                    }
                }
            }
            sizes.push(size);
        }
        (region, sizes)
    }

    /// Eine Rampe von Feld a (Stufe la) nach b (Stufe lb) legen: entlang der
    /// Linie steigt der Boden gleichmäßig, drei Felder breit. Wasser wird zur Furt.
    fn carve_ramp(&mut self, a: (i32, i32), b: (i32, i32)) {
        let la = self.level[self.idx(a.0, a.1)].max(0) as f32;
        let lb = self.level[self.idx(b.0, b.1)].max(0) as f32;
        let (dx, dy) = ((b.0 - a.0) as f32, (b.1 - a.1) as f32);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let steps = (len * 2.0) as i32;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let (px, py) = (a.0 as f32 + dx * t, a.1 as f32 + dy * t);
            let lv = (la + (lb - la) * t).round() as i8;
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let (x, y) = (px.round() as i32 + ox, py.round() as i32 + oy);
                    if self.inside(x, y) {
                        let i = self.idx(x, y);
                        self.level[i] = lv;
                    }
                }
            }
        }
    }

    /// Abgeschnittene Gebiete an das größte anbinden: kleine einebnen, große
    /// über eine Rampe zum nächstgelegenen Feld des Hauptgebiets.
    fn connect(&mut self, rng: &mut Rng) {
        for _round in 0..200 {
            let (region, sizes) = self.regions();
            let Some(main) = (0..sizes.len()).max_by_key(|&r| sizes[r]) else {
                return;
            };
            // Das größte abgeschnittene Gebiet zuerst.
            let Some(other) = (0..sizes.len()).filter(|&r| r != main).max_by_key(|&r| sizes[r]) else {
                return;
            };
            let other = other as u32;
            if sizes[other as usize] < 40 {
                // Nur noch Krümel: auf die Stufe eines Nachbarn setzen bzw. zu Wasser machen.
                for i in 0..self.w * self.h {
                    if region[i] != u32::MAX && region[i] != main as u32 {
                        let (x, y) = ((i % self.w) as i32, (i / self.w) as i32);
                        let nb = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                            .iter()
                            .map(|&(dx, dy)| (x + dx, y + dy))
                            .filter(|&(nx, ny)| self.inside(nx, ny))
                            .map(|(nx, ny)| self.idx(nx, ny))
                            .find(|&j| region[j] == main as u32);
                        self.level[i] = match nb {
                            Some(j) => self.level[j],
                            None => WATER,
                        };
                    }
                }
                continue;
            }
            // Ein zufälliges Feld des abgeschnittenen Gebiets und das nächste Feld
            // des Hauptgebiets dazu (grob über eine Stichprobe).
            let cells: Vec<usize> = (0..self.w * self.h).filter(|&i| region[i] == other).collect();
            let pick = cells[(rng.next() % cells.len() as u64) as usize];
            let (px, py) = ((pick % self.w) as i32, (pick / self.w) as i32);
            let mut best = (i64::MAX, (0, 0));
            for r in 1..(self.w.max(self.h) as i32) {
                for (x, y) in ring(px, py, r) {
                    if self.inside(x, y) && region[self.idx(x, y)] == main as u32 {
                        let d = ((x - px) as i64).pow(2) + ((y - py) as i64).pow(2);
                        if d < best.0 {
                            best = (d, (x, y));
                        }
                    }
                }
                if best.0 != i64::MAX {
                    break;
                }
            }
            if best.0 == i64::MAX {
                return;
            }
            // Vom Hauptgebiet aus: genug Anlauf, damit die Rampe nicht steiler
            // als eine Stufe pro Feld wird.
            let (mx, my) = best.1;
            let diff = (self.level[self.idx(mx, my)].max(0) - self.level[pick].max(0)).unsigned_abs() as f32;
            let (dx, dy) = ((px - mx) as f32, (py - my) as f32);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let need = (diff * 2.5).max(len);
            let a = (
                (px as f32 - dx / len * need).round().clamp(0.0, self.w as f32 - 1.0) as i32,
                (py as f32 - dy / len * need).round().clamp(0.0, self.h as f32 - 1.0) as i32,
            );
            self.carve_ramp(a, (px, py));
        }
    }

    /// Einen Fluss quer über die Karte graben: von einem Rand zum gegenüber
    /// liegenden, mit Schlängeln, an Startplätzen vorbei. Unterwegs ab und zu
    /// eine Furt. Liefert die Furten (Ufer links, Ufer rechts).
    /// Legt einen Fluss an. Liefert die Furten (Ufer-Paare) und den Lauf des Flusses.
    fn river(&mut self, rng: &mut Rng, spawns: &[(i32, i32)], keep_off: f32) -> (Vec<((i32, i32), (i32, i32))>, Vec<(f32, f32)>) {
        let (w, h) = (self.w as f32, self.h as f32);
        let wiggle = Noise { seed: rng.next() };
        let breadth = Noise { seed: rng.next() };
        // Start auf einem zufälligen Rand, Ziel grob gegenüber.
        let side = rng.next() % 4;
        let along = rng.range(0.1, 0.9);
        let across = rng.range(0.1, 0.9);
        let (start, goal) = match side {
            0 => ((along * w, 0.0), (across * w, h - 1.0)),
            1 => ((along * w, h - 1.0), (across * w, 0.0)),
            2 => ((0.0, along * h), (w - 1.0, across * h)),
            _ => ((w - 1.0, along * h), (0.0, across * h)),
        };
        let base_width = (self.w.min(self.h) as f32 / 110.0).clamp(1.6, 3.2);
        let (mut x, mut y) = start;
        let mut path: Vec<(f32, f32)> = Vec::new();
        let max_steps = (w + h) as usize * 3;
        for s in 0..max_steps {
            path.push((x, y));
            let (gx, gy) = (goal.0 - x, goal.1 - y);
            let gd = (gx * gx + gy * gy).sqrt();
            if gd < 2.0 {
                break;
            }
            let mut ang = gy.atan2(gx) + (wiggle.fbm(s as f32, 0.0, 45.0, 3) - 0.5) * 2.6;
            let (mut dx, mut dy) = (ang.cos(), ang.sin());
            // Um Startplätze herum ausweichen.
            for &(sx, sy) in spawns {
                let (ox, oy) = (x - sx as f32, y - sy as f32);
                let d = (ox * ox + oy * oy).sqrt().max(0.1);
                if d < keep_off * 1.6 {
                    let push = ((keep_off * 1.6 - d) / keep_off).min(2.0) * 1.5;
                    dx += ox / d * push;
                    dy += oy / d * push;
                }
            }
            ang = dy.atan2(dx);
            x = (x + ang.cos()).clamp(0.0, w - 1.0);
            y = (y + ang.sin()).clamp(0.0, h - 1.0);
        }
        let mut fords = Vec::new();
        let mut next_ford = rng.range(40.0, 70.0) as usize;
        for (k, &(px, py)) in path.iter().enumerate() {
            let near_spawn =
                spawns.iter().any(|&(sx, sy)| ((px - sx as f32).powi(2) + (py - sy as f32).powi(2)).sqrt() < keep_off);
            if near_spawn {
                continue;
            }
            let r = base_width * (0.7 + 0.6 * breadth.fbm(k as f32, 3.0, 60.0, 2));
            for (cx, cy) in self.disc(px.round() as i32, py.round() as i32, r) {
                let i = self.idx(cx, cy);
                self.level[i] = WATER;
            }
            if k == next_ford && k + 2 < path.len() && k >= 2 {
                next_ford += rng.range(70.0, 130.0) as usize;
                let (ax, ay) = path[k - 2];
                let (bx, by) = path[k + 2];
                let (tx, ty) = (bx - ax, by - ay);
                let tl = (tx * tx + ty * ty).sqrt().max(0.1);
                let (nx, ny) = (-ty / tl, tx / tl);
                let reach = r + 4.0;
                let a = ((px + nx * reach).round() as i32, (py + ny * reach).round() as i32);
                let b = ((px - nx * reach).round() as i32, (py - ny * reach).round() as i32);
                if self.inside(a.0, a.1) && self.inside(b.0, b.1) {
                    fords.push((a, b));
                }
            }
        }
        (fords, path)
    }

    /// Brücke quer über den Fluss am Punkt `p` (Fließrichtung `t`): gerade Reihe Wasser
    /// bis höchstens `MAX_BRIDGE_SPAN`, an beiden Enden begehbares Ufer. Liefert x, y, w, h.
    fn bridge_at(&self, p: (f32, f32), t: (f32, f32)) -> Option<(i32, i32, i32, i32)> {
        let (x, y) = (p.0.round() as i32, p.1.round() as i32);
        if !self.inside(x, y) || self.level[self.idx(x, y)] != WATER {
            return None;
        }
        // Quer zur Fließrichtung, auf das Gitter gerundet.
        let (dx, dy) = if t.0.abs() >= t.1.abs() { (0, 1) } else { (1, 0) };
        let max = crate::map::MAX_BRIDGE_SPAN as i32;
        let wet = |k: i32| {
            let (cx, cy) = (x + dx * k, y + dy * k);
            self.inside(cx, cy) && self.level[self.idx(cx, cy)] == WATER
        };
        let mut lo = 0;
        while wet(lo - 1) && -lo <= max {
            lo -= 1;
        }
        let mut hi = 0;
        while wet(hi + 1) && hi - lo < max {
            hi += 1;
        }
        let len = hi - lo + 1;
        // Ein einzelnes Feld wäre eher eine Pfütze als ein Fluss (und die Richtung unklar).
        if len > max || len < 2 {
            return None;
        }
        let a = (x + dx * (lo - 1), y + dy * (lo - 1));
        let b = (x + dx * (hi + 1), y + dy * (hi + 1));
        let beyond_a = (a.0 - dx, a.1 - dy);
        let beyond_b = (b.0 + dx, b.1 + dy);
        for q in [a, b, beyond_a, beyond_b] {
            if !self.inside(q.0, q.1) {
                return None;
            }
        }
        let (ia, ib) = (self.idx(a.0, a.1), self.idx(b.0, b.1));
        if self.level[ia] == WATER || self.level[ib] == WATER {
            return None;
        }
        // Vom Ufer muss man weiterkommen, und der Belag darf je Feld nur eine Stufe steigen.
        if !self.step_ok(ia, self.idx(beyond_a.0, beyond_a.1)) || !self.step_ok(ib, self.idx(beyond_b.0, beyond_b.1)) {
            return None;
        }
        if (self.level[ia] - self.level[ib]).abs() as i32 > len + 1 {
            return None;
        }
        let (sx, sy) = (x + dx * lo, y + dy * lo);
        Some(if dx == 1 { (sx, sy, len, 1) } else { (sx, sy, 1, len) })
    }

    /// Ressourcen vor beiden Enden der Brücke wegräumen, damit man vom Ufer wegkommt.
    fn clear_bridge_ends(&mut self, (x, y, w, h): (i32, i32, i32, i32)) {
        let (dx, dy) = if w > 1 { (1, 0) } else { (0, 1) };
        let len = w.max(h);
        let ends = [((x - dx, y - dy), (-dx, -dy)), ((x + dx * len, y + dy * len), (dx, dy))];
        for ((ex, ey), (ox, oy)) in ends {
            for k in 0..8 {
                let (cx, cy) = (ex + ox * k, ey + oy * k);
                for side in -1..=1 {
                    let (px, py) = (cx + oy.abs() * side, cy + ox.abs() * side);
                    if self.inside(px, py) {
                        let i = self.idx(px, py);
                        if self.level[i] != WATER {
                            self.res[i] = b'.';
                        }
                    }
                }
            }
        }
    }

    fn disc(&self, cx: i32, cy: i32, r: f32) -> Vec<(i32, i32)> {
        let ri = r.ceil() as i32;
        let mut out = Vec::new();
        for y in cy - ri..=cy + ri {
            for x in cx - ri..=cx + ri {
                let d = (((x - cx).pow(2) + (y - cy).pow(2)) as f32).sqrt();
                if d <= r && self.inside(x, y) {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// Einen runden Fleck einer Ressource legen, wenn dort genug Land frei ist.
    fn blob(&mut self, cx: i32, cy: i32, r: f32, c: u8, core: Option<(f32, u8)>) -> bool {
        let cells = self.disc(cx, cy, r);
        let ok = cells.iter().all(|&(x, y)| {
            let i = self.idx(x, y);
            self.level[i] != WATER && !self.reserved[i] && self.res[i] == b'.'
        });
        if !ok {
            return false;
        }
        for &(x, y) in &cells {
            let i = self.idx(x, y);
            let d = (((x - cx).pow(2) + (y - cy).pow(2)) as f32).sqrt();
            self.res[i] = match core {
                Some((cr, cc)) if d <= cr => cc,
                _ => c,
            };
        }
        true
    }

    /// Fleck irgendwo im Ring um (cx, cy) legen, mehrere Versuche.
    fn blob_near(&mut self, rng: &mut Rng, c: (f32, f32), dist: (f32, f32), r: f32, kind: u8, core: Option<(f32, u8)>) {
        for _ in 0..60 {
            let ang = rng.range(0.0, std::f32::consts::TAU);
            let d = rng.range(dist.0, dist.1);
            let (x, y) = ((c.0 + ang.cos() * d) as i32, (c.1 + ang.sin() * d) as i32);
            if self.blob(x, y, r, kind, core) {
                return;
            }
        }
    }
}

/// Felder auf dem Rand des Quadrats mit Radius r um (cx, cy).
fn ring(cx: i32, cy: i32, r: i32) -> impl Iterator<Item = (i32, i32)> {
    (-r..=r).flat_map(move |k| [(cx + k, cy - r), (cx + k, cy + r), (cx - r, cy + k), (cx + r, cy + k)])
}

/// Karte im Dateiformat erzeugen.
pub fn generate(p: &Params) -> String {
    let (w, h) = (p.width, p.height);
    let mut rng = Rng(p.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x5DEE_CE66);
    let height_noise = Noise { seed: rng.next() };
    let sharp_noise = Noise { seed: rng.next() };
    let forest_noise = Noise { seed: rng.next() };
    let hill_noise = Noise { seed: rng.next() };
    let clamp = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 2.0)
        } else {
            1.0
        }
    };
    let (farmland, forest, stone, iron, hills) =
        (clamp(p.farmland), clamp(p.forest), clamp(p.stone), clamp(p.iron), clamp(p.hills));
    let size = |v: f32| {
        if v.is_finite() {
            v.clamp(SIZE_RANGE.0, SIZE_RANGE.1)
        } else {
            1.0
        }
    };
    let (forest_size, stone_size, iron_size) = (size(p.forest_size), size(p.stone_size), size(p.iron_size));
    // Radius eines Flecks: Grundgröße, über 100 % Menge etwas größer, dazu der Größenregler.
    let stone_r = |f: f32| (grow(3.0, f) * stone_size).max(MIN_DEPOSIT_RADIUS);
    let iron_r = |f: f32| (grow(2.6, f) * iron_size).max(MIN_DEPOSIT_RADIUS);

    // Höhen: Terrassen zu je zwei Stufen. Wo die Kante scharf ist, entsteht
    // eine Wand, sonst eine Schräge.
    let mut g = Gen { w, h, level: vec![0; w * h], res: vec![b'.'; w * h], reserved: vec![false; w * h] };
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            let e = height_noise.fbm(fx, fy, 110.0, 5);
            // Kontrast etwas strecken: Rauschen liegt meist um 0,5.
            let e = ((e - 0.5) * 1.8 + 0.5).clamp(0.0, 1.0);
            let i = y * w + x;
            if e < 0.18 {
                g.level[i] = WATER;
                continue;
            }
            // Hügeligkeit: Höhenunterschiede strecken bzw. stauchen, dazu
            // feinere Hügel obendrauf, wenn es hügeliger werden soll.
            // Um eine mittlere Höhe herum, damit es oben nicht nur Hochebene wird.
            let amp = if hills <= 1.0 { hills } else { 1.0 + 0.6 * (hills - 1.0) };
            let detail = (hill_noise.fbm(fx, fy, 38.0, 3) - 0.5) * (hills - 1.0).max(0.0) * 0.6;
            let u = 0.3 + ((e - 0.18) / 0.82 - 0.3) * amp + detail;
            let t = u.clamp(0.0, 1.0) * (MAX_LEVEL as f32 / 2.0);
            let (fl, fr) = (t.floor(), t - t.floor());
            let width = 0.02 + 0.8 * sharp_noise.fbm(fx, fy, 70.0, 2).powi(3);
            let step = smoothstep(0.5 - width / 2.0, 0.5 + width / 2.0, fr);
            g.level[i] = ((fl + step) * 2.0).round().min(MAX_LEVEL as f32) as i8;
        }
    }

    // Startplätze im Kreis um die Mitte, je ein ebener Platz.
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let radius = cx.min(cy) * if p.players > 1 { 0.72 } else { 0.0 };
    let turn = rng.range(0.0, std::f32::consts::TAU);
    let mut spawns = Vec::new();
    for k in 0..p.players {
        let a = turn + k as f32 / p.players as f32 * std::f32::consts::TAU;
        let (sx, sy) = ((cx + a.cos() * radius) as i32, (cy + a.sin() * radius) as i32);
        spawns.push((sx, sy));
    }
    let flat = 14.0;
    let blend = 10.0;
    let mut fords = Vec::new();
    let mut courses = Vec::new();
    for _ in 0..p.rivers.min(MAX_RIVERS) {
        let (f, course) = g.river(&mut rng, &spawns, flat + blend + 6.0);
        fords.extend(f);
        courses.push(course);
    }
    let ford_mids: Vec<(f32, f32)> =
        fords.iter().map(|&(a, b)| ((a.0 + b.0) as f32 / 2.0, (a.1 + b.1) as f32 / 2.0)).collect();
    for &(sx, sy) in &spawns {
        let lv = g.level[g.idx(sx, sy)].clamp(1, 5);
        for (x, y) in g.disc(sx, sy, flat + blend) {
            let i = g.idx(x, y);
            let d = (((x - sx).pow(2) + (y - sy).pow(2)) as f32).sqrt();
            let t = smoothstep(flat, flat + blend, d);
            let orig = g.level[i].max(0) as f32;
            g.level[i] = (lv as f32 + (orig - lv as f32) * t).round() as i8;
            if d <= flat + 4.0 {
                g.reserved[i] = true;
            }
        }
    }
    for (a, b) in fords {
        // Nur wo beide Ufer Land sind; die Höhe gleicht die Rampe aus.
        if g.level[g.idx(a.0, a.1)] != WATER && g.level[g.idx(b.0, b.1)] != WATER {
            g.carve_ramp(a, b);
        }
    }
    g.connect(&mut rng);

    // Wald: Gruppen aus Rauschen, nicht auf Startplätzen. 0,62 ist normal.
    // Größere Flecken = gröberes Rauschen (gleicher Anteil, aber zusammenhängender).
    let forest_threshold = 0.74 - 0.12 * forest;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if forest > 0.0
                && g.level[i] != WATER
                && !g.reserved[i]
                && forest_noise.fbm(x as f32, y as f32, 40.0 * forest_size, 3) > forest_threshold
            {
                g.res[i] = b'W';
            }
        }
    }
    // Jeder Spieler bekommt in der Nähe Stein, Eisen, Acker und etwas Wald.
    for &(sx, sy) in &spawns {
        let c = (sx as f32, sy as f32);
        for _ in 0..times(&mut rng, 2.0, stone) {
            g.blob_near(&mut rng, c, (22.0, 36.0), stone_r(stone), b'S', None);
        }
        for _ in 0..times(&mut rng, 1.0, iron) {
            g.blob_near(&mut rng, c, (30.0, 44.0), iron_r(iron), b'E', None);
        }
        let fields = times(&mut rng, 2.0, farmland);
        for k in 0..fields {
            if k == 0 {
                let r = grow(5.0, farmland);
                g.blob_near(&mut rng, c, (20.0, 30.0), r, b'A', Some((r / 2.0, b'F')));
            } else {
                g.blob_near(&mut rng, c, (24.0, 36.0), grow(4.5, farmland), b'A', None);
            }
        }
        for _ in 0..times(&mut rng, 3.0, forest) {
            g.blob_near(&mut rng, c, (20.0, 34.0), grow(4.0, forest) * forest_size, b'W', None);
        }
    }
    // Und verstreut über die Karte, je nach Fläche.
    // Je Sorte ein Viertel davon, wie bisher; der Regler skaliert die Anzahl.
    let extra = (w * h / 12_000).max(2) as f32 / 4.0;
    let place = |rng: &mut Rng, g: &mut Gen, f: f32, kind: u8| {
        for _ in 0..times(rng, extra, f) {
            let (x, y) = (rng.range(0.0, w as f32) as i32, rng.range(0.0, h as f32) as i32);
            match kind {
                b'S' => _ = g.blob(x, y, stone_r(f), b'S', None),
                b'E' => _ = g.blob(x, y, iron_r(f), b'E', None),
                b'F' => {
                    let r = grow(5.5, f);
                    let _ = g.blob(x, y, r, b'A', Some((r * 0.55, b'F')));
                }
                _ => _ = g.blob(x, y, grow(4.5, f), b'A', None),
            }
        }
    };
    place(&mut rng, &mut g, stone, b'S');
    place(&mut rng, &mut g, iron, b'E');
    place(&mut rng, &mut g, farmland, b'F');
    place(&mut rng, &mut g, farmland, b'A');

    if let Some(&s0) = spawns.first() {
        g.open_forest_pockets(s0);
    }

    // Brücken: auf jedem Fluss einige, nicht neben Furten und nicht an Startplätzen.
    // Eigener Zufall, damit Gelände und Ressourcen bei gleichem Seed gleich bleiben.
    let mut brng = Rng(p.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xB81D_6E5);
    let keep_off = flat + blend + 6.0;
    let mut bridges: Vec<(i32, i32, i32, i32)> = Vec::new();
    for course in &courses {
        let mut k = brng.range(25.0, 55.0) as usize;
        while k + 3 < course.len() {
            let found = (k..(k + 20).min(course.len() - 3)).find_map(|j| {
                let (px, py) = course[j];
                let near = |q: (f32, f32), r: f32| (q.0 - px).hypot(q.1 - py) < r;
                if spawns.iter().any(|&(sx, sy)| near((sx as f32, sy as f32), keep_off)) || ford_mids.iter().any(|&m| near(m, 25.0)) {
                    return None;
                }
                let (ax, ay) = course[j.saturating_sub(3)];
                let (bx, by) = course[j + 3];
                let rect = g.bridge_at((px, py), (bx - ax, by - ay))?;
                let overlap = bridges.iter().any(|&o| {
                    let grow = |r: (i32, i32, i32, i32)| (r.0 - 3, r.1 - 3, r.2 + 6, r.3 + 6);
                    let (a, b) = (grow(o), rect);
                    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
                });
                (!overlap).then_some(rect)
            });
            if let Some(rect) = found {
                g.clear_bridge_ends(rect);
                bridges.push(rect);
            }
            k += brng.range(70.0, 120.0) as usize;
        }
    }

    let mut out = String::with_capacity((w + 1) * h * 2 + 512);
    let _ = writeln!(out, "# Zufallskarte {w} x {h} für {} Spieler, Seed {}.", p.players, p.seed);
    let _ = writeln!(
        out,
        "# Acker {:.0} %, Wald {:.0} % (Größe {:.0} %), Stein {:.0} % (Größe {:.0} %), Eisen {:.0} % (Größe {:.0} %), Hügel {:.0} %, Flüsse {}.",
        farmland * 100.0,
        forest * 100.0,
        forest_size * 100.0,
        stone * 100.0,
        stone_size * 100.0,
        iron * 100.0,
        iron_size * 100.0,
        hills * 100.0,
        p.rivers.min(MAX_RIVERS)
    );
    let _ = writeln!(out, "# Erzeugt vom Kartengenerator; Format siehe server/maps/zwei-burgen.txt.");
    for (x, y) in &spawns {
        let _ = writeln!(out, "spawn {x} {y}");
    }
    for (x, y, bw, bh) in &bridges {
        let _ = writeln!(out, "bridge {x} {y} {bw} {bh}");
    }
    out.push_str("[terrain]\n");
    for y in 0..h {
        for x in 0..w {
            let l = g.level[y * w + x];
            out.push(if l == WATER { '~' } else { (b'0' + l as u8) as char });
        }
        out.push('\n');
    }
    out.push_str("[resources]\n");
    for y in 0..h {
        out.push_str(std::str::from_utf8(&g.res[y * w..(y + 1) * w]).unwrap());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::GameMap;

    fn check(size: usize, players: usize, seed: u64) -> GameMap {
        let t = std::time::Instant::now();
        let text = generate(&Params::new(size, size, players, seed));
        let gen_ms = t.elapsed().as_millis();
        let t = std::time::Instant::now();
        let result = GameMap::load(&text);
        let load_ms = t.elapsed().as_millis();
        assert!(result.errors.is_empty(), "{size}: {:?}", result.errors.first());
        let map = result.map.unwrap();
        assert_eq!(map.spawns.len(), players);
        assert!(result.warnings.is_empty(), "{size}: {:?}", result.warnings.first());
        // Alle Startplätze sind untereinander erreichbar.
        let s0 = map.spawns[0];
        let t = std::time::Instant::now();
        for &s in &map.spawns[1..] {
            let path = map.nav.find_path((s0.0 as i32, s0.1 as i32), (s.0 as i32, s.1 as i32));
            assert_eq!(path.last(), Some(&(s.0 as i32, s.1 as i32)), "{size}: Spawn {s:?} nicht erreichbar");
        }
        let path_ms = t.elapsed().as_millis();
        eprintln!(
            "{size}×{size}, {players} Spieler: erzeugen {gen_ms} ms, laden+prüfen {load_ms} ms, \
             {} Wege {path_ms} ms, {} Hinweise",
            players - 1,
            result.warnings.len()
        );
        map
    }

    #[test]
    fn karten_256_und_512() {
        check(256, 2, 1);
        check(256, 4, 7);
        check(512, 6, 3);
    }

    #[test]
    fn gleicher_seed_gleiche_karte() {
        let p = Params::new(64, 64, 2, 42);
        assert_eq!(generate(&p), generate(&p));
    }

    #[test]
    fn regler_extreme() {
        for (f, hills, rivers, sz) in [(0.0, 0.0, 0, 0.5), (2.0, 2.0, 4, 3.0), (0.5, 1.5, 8, 1.0), (1.0, 1.0, 1, 3.0)] {
            let p = Params {
                farmland: f,
                forest: f,
                stone: f,
                iron: f,
                forest_size: sz,
                stone_size: sz,
                iron_size: sz,
                hills,
                rivers,
                ..Params::new(256, 256, 4, 11)
            };
            let text = generate(&p);
            let result = GameMap::load(&text);
            assert!(result.errors.is_empty(), "{f}/{hills}/{rivers}: {:?}", result.errors.first());
            let map = result.map.unwrap();
            let s0 = map.spawns[0];
            for &s in &map.spawns[1..] {
                let path = map.nav.find_path((s0.0 as i32, s0.1 as i32), (s.0 as i32, s.1 as i32));
                assert_eq!(path.last(), Some(&(s.0 as i32, s.1 as i32)), "{f}/{hills}/{rivers}: {s:?}");
            }
        }
    }

    /// Durchschnittliche Felder je zusammenhängendem Fleck einer Ressource.
    fn mean_patch(text: &str, c: u8) -> f32 {
        let rows: Vec<&[u8]> = text.split("[resources]\n").nth(1).unwrap().lines().map(str::as_bytes).collect();
        let (h, w) = (rows.len(), rows[0].len());
        let mut seen = vec![false; w * h];
        let (mut patches, mut cells) = (0, 0);
        for y in 0..h {
            for x in 0..w {
                if rows[y][x] != c || seen[y * w + x] {
                    continue;
                }
                patches += 1;
                let mut stack = vec![(x, y)];
                seen[y * w + x] = true;
                while let Some((x, y)) = stack.pop() {
                    cells += 1;
                    for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let (nx, ny) = (nx as usize, ny as usize);
                        if rows[ny][nx] == c && !seen[ny * w + nx] {
                            seen[ny * w + nx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
        }
        cells as f32 / patches.max(1) as f32
    }

    #[test]
    fn groessere_flecken() {
        let small = generate(&Params { forest_size: 0.5, stone_size: 0.5, iron_size: 0.5, ..Params::new(256, 256, 2, 5) });
        let big = generate(&Params { forest_size: 2.5, stone_size: 2.5, iron_size: 2.5, ..Params::new(256, 256, 2, 5) });
        for c in [b'W', b'S', b'E'] {
            let (a, b) = (mean_patch(&small, c), mean_patch(&big, c));
            assert!(b > a * 1.5, "{}: klein {a}, groß {b}", c as char);
        }
    }

    #[test]
    fn mehr_regler_mehr_ressourcen() {
        let count = |text: &str, c: char| text.split("[resources]").nth(1).unwrap().matches(c).count();
        let low = generate(&Params { stone: 0.3, iron: 0.3, forest: 0.3, ..Params::new(256, 256, 2, 5) });
        let high = generate(&Params { stone: 2.0, iron: 2.0, forest: 2.0, ..Params::new(256, 256, 2, 5) });
        for c in ['S', 'E', 'W'] {
            assert!(count(&high, c) > count(&low, c), "{c}");
        }
        let dry = generate(&Params { rivers: 0, ..Params::new(256, 256, 2, 5) });
        let wet = generate(&Params { rivers: 4, ..Params::new(256, 256, 2, 5) });
        let water = |t: &str| t.split("[resources]").next().unwrap().matches('~').count();
        assert!(water(&wet) > water(&dry));
    }

    #[test]
    fn fluesse_bekommen_bruecken() {
        let mut total = 0;
        for seed in 1..6 {
            let text = generate(&Params { rivers: 2, ..Params::new(256, 256, 2, seed) });
            let r = GameMap::load(&text);
            assert!(r.errors.is_empty(), "{seed}: {:?}", r.errors.first());
            let map = r.map.unwrap();
            for &(x, y, w, h) in &map.bridges {
                assert!(w.max(h) <= crate::map::MAX_BRIDGE_SPAN && w.min(h) == 1);
                assert!(map.nav.has_deck(x as i32, y as i32));
            }
            total += map.bridges.len();
        }
        assert!(total >= 5, "nur {total} Brücken auf 5 Karten mit je 2 Flüssen");
    }

    /// Dauert im Debug-Build einige Sekunden: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn karte_1000_mit_8_spielern() {
        check(1000, 8, 5);
    }
}
