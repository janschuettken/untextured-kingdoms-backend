//! Zufallskarten beliebiger Größe (bis 1024 × 1024) für bis zu 8 Spieler.
//!
//! Ablauf: Höhen aus Rauschen mit Terrassen (mal sanfte Schrägen, mal Wände),
//! Seen in den Senken, ebene Startplätze im Kreis, danach werden abgeschnittene
//! Gebiete mit Rampen (bzw. Furten) angebunden. Zuletzt Ressourcen: Wald in
//! Gruppen, Stein- und Eisenvorkommen ab 5 × 5 Feldern, Äcker ab 7 × 7.
//!
//! Gleicher Seed, gleiche Karte.

use std::fmt::Write;

use crate::map::MAX_LEVEL;

pub struct Params {
    pub width: usize,
    pub height: usize,
    pub players: usize,
    pub seed: u64,
}

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
            // zwei Felder breite Schneise schlagen.
            let mut came = vec![usize::MAX; n];
            let mut queue = std::collections::VecDeque::from([pocket]);
            came[pocket] = pocket;
            let mut hit = None;
            while let Some(i) = queue.pop_front() {
                if reach[i] {
                    hit = Some(i);
                    break;
                }
                for j in self.neighbors4(i) {
                    if came[j] == usize::MAX && self.res[j] != b'S' && self.res[j] != b'E' && self.step_ok(i, j) {
                        came[j] = i;
                        queue.push_back(j);
                    }
                }
            }
            let Some(mut i) = hit else {
                for c in cells {
                    self.res[c] = b'W';
                }
                continue;
            };
            while i != pocket {
                for j in std::iter::once(i).chain(self.neighbors4(i).into_iter().take(2)) {
                    if self.res[j] == b'W' {
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
                    if region[j] == u32::MAX
                        && self.level[j] != WATER
                        && (self.level[j] - self.level[i]).abs() <= 1
                    {
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
            let Some(main) = (0..sizes.len()).max_by_key(|&r| sizes[r]) else { return };
            // Das größte abgeschnittene Gebiet zuerst.
            let Some(other) = (0..sizes.len()).filter(|&r| r != main).max_by_key(|&r| sizes[r]) else { return };
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
            let t = (e - 0.18) / 0.82 * (MAX_LEVEL as f32 / 2.0);
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
    g.connect(&mut rng);

    // Wald: Gruppen aus Rauschen, nicht auf Startplätzen.
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if g.level[i] != WATER && !g.reserved[i] && forest_noise.fbm(x as f32, y as f32, 40.0, 3) > 0.62 {
                g.res[i] = b'W';
            }
        }
    }
    // Jeder Spieler bekommt in der Nähe Stein, Eisen, Acker und etwas Wald.
    for &(sx, sy) in &spawns {
        let c = (sx as f32, sy as f32);
        g.blob_near(&mut rng, c, (22.0, 32.0), 3.0, b'S', None);
        g.blob_near(&mut rng, c, (24.0, 36.0), 3.0, b'S', None);
        g.blob_near(&mut rng, c, (30.0, 44.0), 2.6, b'E', None);
        g.blob_near(&mut rng, c, (20.0, 30.0), 5.0, b'A', Some((2.5, b'F')));
        g.blob_near(&mut rng, c, (24.0, 36.0), 4.5, b'A', None);
        for _ in 0..3 {
            g.blob_near(&mut rng, c, (20.0, 34.0), 4.0, b'W', None);
        }
    }
    // Und verstreut über die Karte, je nach Fläche.
    let extra = (w * h / 12_000).max(2);
    for _ in 0..extra {
        let (x, y) = (rng.range(0.0, w as f32) as i32, rng.range(0.0, h as f32) as i32);
        match rng.next() % 4 {
            0 => drop(g.blob(x, y, 3.0, b'S', None)),
            1 => drop(g.blob(x, y, 2.6, b'E', None)),
            2 => drop(g.blob(x, y, 5.5, b'A', Some((3.0, b'F')))),
            _ => drop(g.blob(x, y, 4.5, b'A', None)),
        }
    }

    if let Some(&s0) = spawns.first() {
        g.open_forest_pockets(s0);
    }

    let mut out = String::with_capacity((w + 1) * h * 2 + 512);
    let _ = writeln!(out, "# Zufallskarte {w} x {h} für {} Spieler, Seed {}.", p.players, p.seed);
    let _ = writeln!(out, "# Erzeugt vom Kartengenerator; Format siehe server/maps/zwei-burgen.txt.");
    for (x, y) in &spawns {
        let _ = writeln!(out, "spawn {x} {y}");
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
        let text = generate(&Params { width: size, height: size, players, seed });
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
        let p = Params { width: 64, height: 64, players: 2, seed: 42 };
        assert_eq!(generate(&p), generate(&p));
    }

    /// Dauert im Debug-Build einige Sekunden: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn karte_1000_mit_8_spielern() {
        check(1000, 8, 5);
    }
}
