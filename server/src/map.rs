//! Karten: werden aus einer Textdatei gelesen (siehe `maps/*.txt`).
//!
//! Aus den Zeichen der Datei entsteht für jedes Feld eine Art (Boden, Rampe,
//! Wasser), eine logische Höhenstufe und vier Eckhöhen. Rampen steigen
//! gleichmäßig über ihre ganze Länge an. Überall sonst, wo zwei Felder an
//! ihrer gemeinsamen Kante unterschiedlich hoch sind, ist eine Klippe.

use protocol::{MapInfo, TileKind};

use crate::nav::NavGrid;

/// Höhenschritte pro Stufe. 12 lässt sich durch 1, 2, 3, 4 und 6 teilen,
/// so steigen Rampen dieser Längen in ganzen Schritten an.
pub const STEPS_PER_LEVEL: u16 = 12;

/// Die Standardkarte, beim Kompilieren eingebettet.
pub const DEFAULT_MAP: &str = include_str!("../maps/zwei-burgen.txt");

/// Eine geladene Karte mit allem, was der Server zur Laufzeit braucht.
pub struct GameMap {
    pub width: u32,
    pub height: u32,
    pub max_level: u8,
    pub tiles: Vec<TileKind>,
    pub levels: Vec<u8>,
    /// Vier Eckhöhen pro Feld: NW, NO, SO, SW (in Schritten).
    pub corners: Vec<[u16; 4]>,
    pub nav: NavGrid,
    /// Startfelder der Spieler.
    pub spawns: Vec<(u32, u32)>,
}

/// Zeichen einer Rampe und ihre Steigungsrichtung.
fn ramp_dir(c: char) -> Option<(i32, i32)> {
    match c {
        '>' => Some((1, 0)),
        '<' => Some((-1, 0)),
        'v' => Some((0, 1)),
        '^' => Some((0, -1)),
        _ => None,
    }
}

impl GameMap {
    /// Liest eine Karte aus dem Textformat. Fehler beschreiben, welches Feld nicht passt.
    pub fn parse(text: &str) -> Result<GameMap, String> {
        let mut rows: Vec<Vec<char>> = Vec::new();
        let mut spawns = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("spawn ") {
                let nums: Vec<u32> = rest
                    .split_whitespace()
                    .map(|s| s.parse().map_err(|_| format!("Zeile {}: ungültiger Spawn", n + 1)))
                    .collect::<Result<_, _>>()?;
                let [x, y] = nums[..] else {
                    return Err(format!("Zeile {}: spawn braucht x und y", n + 1));
                };
                spawns.push((x, y));
                continue;
            }
            rows.push(line.chars().collect());
        }

        let height = rows.len() as u32;
        let width = rows.first().map_or(0, |r| r.len()) as u32;
        if width == 0 || rows.iter().any(|r| r.len() as u32 != width) {
            return Err("Alle Kartenzeilen müssen gleich lang sein".into());
        }
        let (w, h) = (width as i32, height as i32);
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        let at = |x: i32, y: i32| -> Option<char> {
            (x >= 0 && y >= 0 && x < w && y < h).then(|| rows[y as usize][x as usize])
        };
        let ground_level = |x: i32, y: i32| at(x, y).and_then(|c| c.to_digit(10)).map(|d| d as u8);

        let n = (width * height) as usize;
        let mut tiles = vec![TileKind::Ground; n];
        let mut levels = vec![0u8; n];
        let mut corners = vec![[0u16; 4]; n];

        // 1. Boden und Wasser markieren, Zeichen prüfen.
        for y in 0..h {
            for x in 0..w {
                let c = rows[y as usize][x as usize];
                let i = idx(x, y);
                if let Some(l) = c.to_digit(10) {
                    levels[i] = l as u8;
                    corners[i] = [l as u16 * STEPS_PER_LEVEL; 4];
                } else if c == '~' {
                    tiles[i] = TileKind::Water;
                } else if ramp_dir(c).is_some() {
                    tiles[i] = TileKind::Ramp;
                } else {
                    return Err(format!("Feld ({x}, {y}): unbekanntes Zeichen '{c}'"));
                }
            }
        }

        // 2. Rampen: jede Reihe gleicher Pfeile in Pfeilrichtung ist eine Rampe.
        for y in 0..h {
            for x in 0..w {
                let c = rows[y as usize][x as usize];
                let Some((dx, dy)) = ramp_dir(c) else { continue };
                // Nur am unteren Ende einer Rampe anfangen.
                if at(x - dx, y - dy) == Some(c) {
                    continue;
                }
                let mut len = 0;
                while at(x + dx * len, y + dy * len) == Some(c) {
                    len += 1;
                }
                let low = ground_level(x - dx, y - dy).ok_or_else(|| {
                    format!("Rampe bei ({x}, {y}): unteres Ende grenzt nicht an Boden")
                })?;
                let (hx, hy) = (x + dx * len, y + dy * len);
                let high = ground_level(hx, hy).ok_or_else(|| {
                    format!("Rampe bei ({x}, {y}): oberes Ende ({hx}, {hy}) grenzt nicht an Boden")
                })?;
                if high <= low {
                    return Err(format!(
                        "Rampe bei ({x}, {y}): oben (Stufe {high}) muss höher sein als unten (Stufe {low})"
                    ));
                }
                let rise = (high - low) as u16 * STEPS_PER_LEVEL;
                if !rise.is_multiple_of(len as u16) {
                    return Err(format!(
                        "Rampe bei ({x}, {y}): Länge {len} passt nicht zu {rise} Höhenschritten"
                    ));
                }
                let step = rise / len as u16;
                for k in 0..len {
                    let i = idx(x + dx * k, y + dy * k);
                    let lo = low as u16 * STEPS_PER_LEVEL + step * k as u16;
                    let hi = lo + step;
                    levels[i] = low;
                    // Ecken auf der ansteigenden Seite bekommen die obere Höhe.
                    corners[i] = match (dx, dy) {
                        (1, 0) => [lo, hi, hi, lo],
                        (-1, 0) => [hi, lo, lo, hi],
                        (0, 1) => [lo, lo, hi, hi],
                        _ => [hi, hi, lo, lo],
                    };
                }
            }
        }

        // 3. Wasser: jede zusammenhängende Wasserfläche liegt auf der
        //    niedrigsten Stufe des angrenzenden Bodens.
        let mut seen = vec![false; n];
        for start in 0..n {
            if tiles[start] != TileKind::Water || seen[start] {
                continue;
            }
            let mut body = vec![start];
            let mut stack = vec![start];
            seen[start] = true;
            let mut level = u8::MAX;
            while let Some(i) = stack.pop() {
                let (x, y) = ((i as i32) % w, (i as i32) / w);
                for (nx, ny) in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
                    if at(nx, ny).is_none() {
                        continue;
                    }
                    let j = idx(nx, ny);
                    match tiles[j] {
                        TileKind::Water if !seen[j] => {
                            seen[j] = true;
                            body.push(j);
                            stack.push(j);
                        }
                        TileKind::Ground => level = level.min(levels[j]),
                        _ => {}
                    }
                }
            }
            if level == u8::MAX {
                level = 0;
            }
            for i in body {
                levels[i] = level;
                corners[i] = [level as u16 * STEPS_PER_LEVEL; 4];
            }
        }

        for &(x, y) in &spawns {
            if x >= width || y >= height || tiles[(y * width + x) as usize] == TileKind::Water {
                return Err(format!("Spawn ({x}, {y}) liegt nicht auf begehbarem Boden"));
            }
        }

        let max_level = levels.iter().copied().max().unwrap_or(0);
        let nav = NavGrid::build(width, height, &tiles, &corners);
        Ok(GameMap { width, height, max_level, tiles, levels, corners, nav, spawns })
    }

    /// Bodenhöhe in Stufen an einer beliebigen Position (für Hochgelände-Boni später).
    #[allow(dead_code)]
    pub fn height_at(&self, x: f32, y: f32) -> f32 {
        let tx = (x.floor() as i32).clamp(0, self.width as i32 - 1);
        let ty = (y.floor() as i32).clamp(0, self.height as i32 - 1);
        let [nw, ne, se, sw] = self.corners[(ty as u32 * self.width + tx as u32) as usize];
        let (fx, fy) = ((x - tx as f32).clamp(0.0, 1.0), (y - ty as f32).clamp(0.0, 1.0));
        let top = nw as f32 + (ne as f32 - nw as f32) * fx;
        let bottom = sw as f32 + (se as f32 - sw as f32) * fx;
        (top + (bottom - top) * fy) / STEPS_PER_LEVEL as f32
    }

    /// Die Karte so, wie sie an die Clients geht.
    pub fn info(&self) -> MapInfo {
        MapInfo {
            width: self.width,
            height: self.height,
            max_level: self.max_level,
            steps_per_level: STEPS_PER_LEVEL,
            tiles: self.tiles.clone(),
            levels: self.levels.clone(),
            corners: self.corners.iter().flatten().copied().collect(),
            nav: self.nav.dirs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standardkarte_ist_gueltig() {
        let map = GameMap::parse(DEFAULT_MAP).expect("Karte lädt");
        assert_eq!((map.width, map.height), (64, 64));
        assert!(map.spawns.len() >= 2);
    }

    #[test]
    fn rampe_steigt_gleichmaessig() {
        let map = GameMap::parse("0>>>1\n0>>>1\n").unwrap();
        // Drei Felder für eine Stufe: je 4 Schritte.
        assert_eq!(map.corners[1], [0, 4, 4, 0]);
        assert_eq!(map.corners[2], [4, 8, 8, 4]);
        assert_eq!(map.corners[3], [8, 12, 12, 8]);
        assert_eq!(map.tiles[2], TileKind::Ramp);
        assert!((map.height_at(2.5, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn falsche_rampen_werden_erkannt() {
        assert!(GameMap::parse("0>>>0\n").is_err(), "oben nicht höher");
        assert!(GameMap::parse("0>>>>>1\n").is_err(), "Länge 5 teilt 12 nicht");
        assert!(GameMap::parse("~>1\n").is_err(), "unten kein Boden");
    }

    #[test]
    fn wasser_liegt_auf_umgebender_stufe() {
        let map = GameMap::parse("111\n1~1\n111\n").unwrap();
        assert_eq!(map.levels[4], 1);
        assert_eq!(map.corners[4], [12; 4]);
    }
}
