//! Karten: werden aus einer Textdatei gelesen (siehe `maps/*.txt`).
//!
//! Aus den Zeichen der Datei entsteht für jedes Feld eine Art (Boden, Rampe,
//! Wasser), eine logische Höhenstufe und vier Eckhöhen. Rampen steigen
//! gleichmäßig über ihre ganze Länge an. Überall sonst, wo zwei Felder an
//! ihrer gemeinsamen Kante unterschiedlich hoch sind, ist eine Klippe.
//!
//! Aufbau der Datei:
//! ```text
//! # Kommentar
//! spawn 10 10
//! [terrain]
//! 0001>>>2
//! ...
//! [resources]        (optional, gleiche Größe wie [terrain])
//! ..WW....
//! ```
//! Ohne Abschnittsüberschriften ist alles `[terrain]` (altes Format).
//!
//! [`GameMap::load`] sammelt alle Fehler und liefert trotzdem eine Karte,
//! damit der Editor sie anzeigen kann. Kaputte Rampen werden dabei zu Boden.

use protocol::{MapCheck, MapInfo, MapIssue, ResourceKind, TileKind};

use crate::nav::NavGrid;

/// Höhenschritte pro Stufe. 12 lässt sich durch 1, 2, 3, 4 und 6 teilen,
/// so steigen Rampen dieser Längen in ganzen Schritten an.
pub const STEPS_PER_LEVEL: u16 = 12;

/// Die Standardkarte, beim Kompilieren eingebettet (Rückfall, falls der
/// Kartenordner fehlt).
pub const DEFAULT_MAP: &str = include_str!("../maps/zwei-burgen.txt");

/// Größte erlaubte Kartenseite.
pub const MAX_SIZE: usize = 256;

/// Eine geladene Karte mit allem, was der Server zur Laufzeit braucht.
pub struct GameMap {
    pub width: u32,
    pub height: u32,
    pub max_level: u8,
    pub tiles: Vec<TileKind>,
    pub resources: Vec<ResourceKind>,
    pub levels: Vec<u8>,
    /// Vier Eckhöhen pro Feld: NW, NO, SO, SW (in Schritten).
    pub corners: Vec<[u16; 4]>,
    pub nav: NavGrid,
    /// Startfelder der Spieler.
    pub spawns: Vec<(u32, u32)>,
}

/// Blockiert diese Ressource das Laufen (bis sie abgebaut ist)?
pub fn resource_blocks(r: ResourceKind) -> bool {
    matches!(r, ResourceKind::Forest | ResourceKind::Stone | ResourceKind::Iron)
}

/// Ergebnis von [`GameMap::load`].
pub struct LoadResult {
    pub map: Option<GameMap>,
    pub errors: Vec<MapIssue>,
    pub warnings: Vec<MapIssue>,
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

pub fn resource_from_char(c: char) -> Option<ResourceKind> {
    Some(match c {
        '.' => ResourceKind::None,
        'W' => ResourceKind::Forest,
        'S' => ResourceKind::Stone,
        'E' => ResourceKind::Iron,
        'A' => ResourceKind::Farmland,
        'F' => ResourceKind::Fertile,
        _ => return None,
    })
}

fn issue(x: i32, y: i32, message: impl Into<String>) -> MapIssue {
    MapIssue { x: Some(x as u32), y: Some(y as u32), message: message.into() }
}

fn general(message: impl Into<String>) -> MapIssue {
    MapIssue { x: None, y: None, message: message.into() }
}

impl GameMap {
    /// Liest eine Karte und bricht beim ersten Fehler ab (für den Spielserver).
    pub fn parse(text: &str) -> Result<GameMap, String> {
        let r = GameMap::load(text);
        if let Some(e) = r.errors.first() {
            let at = match (e.x, e.y) {
                (Some(x), Some(y)) => format!("Feld ({x}, {y}): "),
                _ => String::new(),
            };
            return Err(format!("{at}{}", e.message));
        }
        r.map.ok_or_else(|| "Karte ist leer".into())
    }

    /// Liest eine Karte und sammelt dabei alle Fehler und Hinweise.
    pub fn load(text: &str) -> LoadResult {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut terrain: Vec<Vec<char>> = Vec::new();
        let mut res_rows: Vec<(usize, Vec<char>)> = Vec::new();
        let mut spawns = Vec::new();
        let mut section = "terrain";

        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                match name {
                    "terrain" | "resources" => section = if name == "terrain" { "terrain" } else { "resources" },
                    _ => errors.push(general(format!("Zeile {}: unbekannter Abschnitt [{name}]", n + 1))),
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("spawn ") {
                let nums: Vec<u32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                match nums[..] {
                    [x, y] => spawns.push((x, y)),
                    _ => errors.push(general(format!("Zeile {}: spawn braucht x und y", n + 1))),
                }
                continue;
            }
            match section {
                "terrain" => terrain.push(line.chars().collect()),
                _ => res_rows.push((n + 1, line.chars().collect())),
            }
        }

        let height = terrain.len();
        let width = terrain.first().map_or(0, |r| r.len());
        if width == 0 {
            errors.push(general("Die Karte hat keine Felder"));
            return LoadResult { map: None, errors, warnings };
        }
        if width > MAX_SIZE || height > MAX_SIZE {
            errors.push(general(format!("Karte ist größer als {MAX_SIZE} x {MAX_SIZE}")));
            return LoadResult { map: None, errors, warnings };
        }
        if let Some(y) = terrain.iter().position(|r| r.len() != width) {
            errors.push(general(format!(
                "Terrain-Zeile {} hat {} statt {width} Zeichen. Alle Zeilen müssen gleich lang sein.",
                y + 1,
                terrain[y].len()
            )));
            return LoadResult { map: None, errors, warnings };
        }

        let (w, h) = (width as i32, height as i32);
        let n = width * height;
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h;

        let mut tiles = vec![TileKind::Ground; n];
        let mut levels = vec![0u8; n];
        let mut corners = vec![[0u16; 4]; n];
        // Zeichen, wie sie verarbeitet werden (kaputte Felder werden ersetzt).
        let mut chars: Vec<Vec<char>> = terrain.clone();

        // 1. Zeichen prüfen.
        for y in 0..h {
            for x in 0..w {
                let c = chars[y as usize][x as usize];
                if !(c.is_ascii_digit() || c == '~' || ramp_dir(c).is_some()) {
                    errors.push(issue(x, y, format!("unbekanntes Zeichen '{c}'")));
                    chars[y as usize][x as usize] = '0';
                }
            }
        }
        let ground_level = |chars: &Vec<Vec<char>>, x: i32, y: i32| {
            if !inside(x, y) {
                return None;
            }
            chars[y as usize][x as usize].to_digit(10).map(|d| d as u8)
        };

        // 2. Boden und Wasser.
        for y in 0..h {
            for x in 0..w {
                let c = chars[y as usize][x as usize];
                let i = idx(x, y);
                if let Some(l) = c.to_digit(10) {
                    levels[i] = l as u8;
                    corners[i] = [l as u16 * STEPS_PER_LEVEL; 4];
                } else if c == '~' {
                    tiles[i] = TileKind::Water;
                }
            }
        }

        // 3. Rampen: jede Reihe gleicher Pfeile in Pfeilrichtung ist eine Rampe.
        for y in 0..h {
            for x in 0..w {
                let c = chars[y as usize][x as usize];
                let Some((dx, dy)) = ramp_dir(c) else { continue };
                let at = |x: i32, y: i32| inside(x, y).then(|| chars[y as usize][x as usize]);
                // Nur am unteren Ende einer Rampe anfangen.
                if at(x - dx, y - dy) == Some(c) {
                    continue;
                }
                let mut len = 0;
                while at(x + dx * len, y + dy * len) == Some(c) {
                    len += 1;
                }
                let (hx, hy) = (x + dx * len, y + dy * len);
                let low = ground_level(&chars, x - dx, y - dy);
                let high = ground_level(&chars, hx, hy);
                let problem = match (low, high) {
                    (None, _) => Some("Rampe: unteres Ende grenzt nicht an Boden".to_string()),
                    (_, None) => Some("Rampe: oberes Ende grenzt nicht an Boden".to_string()),
                    (Some(lo), Some(hi)) if hi <= lo => Some(format!(
                        "Rampe: oben (Stufe {hi}) muss höher sein als unten (Stufe {lo})"
                    )),
                    (Some(lo), Some(hi))
                        if !((hi - lo) as u16 * STEPS_PER_LEVEL).is_multiple_of(len as u16) =>
                    {
                        Some(format!(
                            "Rampe: Länge {len} passt nicht. Pro Stufe 1, 2, 3, 4 oder 6 Felder lang."
                        ))
                    }
                    _ => None,
                };
                if let Some(msg) = problem {
                    errors.push(issue(x, y, msg));
                    // Zum Weiterzeigen: Rampe wird flacher Boden auf der unteren Stufe.
                    let l = low.or(high).unwrap_or(0);
                    for k in 0..len {
                        let i = idx(x + dx * k, y + dy * k);
                        tiles[i] = TileKind::Ground;
                        levels[i] = l;
                        corners[i] = [l as u16 * STEPS_PER_LEVEL; 4];
                    }
                    continue;
                }
                let (low, high) = (low.unwrap(), high.unwrap());
                let step = (high - low) as u16 * STEPS_PER_LEVEL / len as u16;
                for k in 0..len {
                    let i = idx(x + dx * k, y + dy * k);
                    let lo = low as u16 * STEPS_PER_LEVEL + step * k as u16;
                    let hi = lo + step;
                    tiles[i] = TileKind::Ramp;
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

        // 4. Wasser: jede zusammenhängende Wasserfläche liegt auf der
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
                    if !inside(nx, ny) {
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

        // 5. Ressourcen-Ebene.
        let mut resources = vec![ResourceKind::None; n];
        if !res_rows.is_empty() {
            if res_rows.len() != height {
                errors.push(general(format!(
                    "[resources] hat {} Zeilen, [terrain] aber {height}",
                    res_rows.len()
                )));
            }
            for (y, (line_no, row)) in res_rows.iter().enumerate().take(height) {
                if row.len() != width {
                    errors.push(general(format!(
                        "Zeile {line_no}: [resources] hat {} statt {width} Zeichen",
                        row.len()
                    )));
                }
                for (x, &c) in row.iter().enumerate().take(width) {
                    let (xi, yi) = (x as i32, y as i32);
                    match resource_from_char(c) {
                        None => errors.push(issue(xi, yi, format!("unbekannte Ressource '{c}'"))),
                        Some(ResourceKind::None) => {}
                        Some(r) => {
                            let i = idx(xi, yi);
                            if tiles[i] != TileKind::Ground {
                                errors.push(issue(xi, yi, "Ressourcen nur auf flachem Boden"));
                            } else {
                                resources[i] = r;
                            }
                        }
                    }
                }
            }
        }

        // 6. Startfelder.
        let mut valid_spawns = Vec::new();
        for &(x, y) in &spawns {
            let ok = (x as usize) < width
                && (y as usize) < height
                && tiles[y as usize * width + x as usize] != TileKind::Water
                && !resource_blocks(resources[y as usize * width + x as usize]);
            if ok {
                valid_spawns.push((x, y));
            } else {
                errors.push(MapIssue {
                    x: ((x as usize) < width).then_some(x),
                    y: ((y as usize) < height).then_some(y),
                    message: format!("Startfeld ({x}, {y}) liegt nicht auf begehbarem Boden"),
                });
            }
        }
        if valid_spawns.len() < 2 {
            warnings.push(general("Weniger als zwei Startfelder: Mehrspieler geht so nicht"));
        }

        let blocked: Vec<bool> = resources.iter().map(|&r| resource_blocks(r)).collect();
        let nav = NavGrid::build(width as u32, height as u32, &tiles, &corners, &blocked);
        let max_level = levels.iter().copied().max().unwrap_or(0);
        let map = GameMap {
            width: width as u32,
            height: height as u32,
            max_level,
            tiles,
            resources,
            levels,
            corners,
            nav,
            spawns: valid_spawns,
        };
        warnings.extend(map.reachability_warnings());
        LoadResult { map: Some(map), errors, warnings }
    }

    /// Begehbare Gebiete, die vom ersten Startfeld aus nicht erreichbar sind.
    /// Eine Meldung pro abgeschnittenem Gebiet.
    fn reachability_warnings(&self) -> Vec<MapIssue> {
        let Some(&(sx, sy)) = self.spawns.first() else { return Vec::new() };
        let (w, h) = (self.width as i32, self.height as i32);
        let n = (self.width * self.height) as usize;
        let mut region = vec![usize::MAX; n];
        let mut regions = Vec::new(); // (Größe, Beispielfeld)
        let flood = |start: (i32, i32), id: usize, region: &mut Vec<usize>| {
            let mut stack = vec![start];
            region[(start.1 * w + start.0) as usize] = id;
            let mut size = 0;
            while let Some((x, y)) = stack.pop() {
                size += 1;
                for &(dx, dy) in &crate::nav::DIRS {
                    let (nx, ny) = (x + dx, y + dy);
                    if self.nav.can_step(x, y, dx, dy) && region[(ny * w + nx) as usize] == usize::MAX {
                        region[(ny * w + nx) as usize] = id;
                        stack.push((nx, ny));
                    }
                }
            }
            size
        };
        flood((sx as i32, sy as i32), 0, &mut region);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if region[i] == usize::MAX && self.nav.is_open(x, y) {
                    let id = regions.len() + 1;
                    let size = flood((x, y), id, &mut region);
                    regions.push((size, (x, y)));
                }
            }
        }
        regions
            .into_iter()
            .map(|(size, (x, y))| {
                issue(x, y, format!("{size} Feld(er) hier sind vom ersten Startfeld aus nicht erreichbar"))
            })
            .collect()
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
            resources: self.resources.clone(),
            levels: self.levels.clone(),
            corners: self.corners.iter().flatten().copied().collect(),
            nav: self.nav.dirs.clone(),
        }
    }
}

impl LoadResult {
    pub fn into_check(self) -> MapCheck {
        MapCheck {
            spawns: self.map.as_ref().map(|m| m.spawns.clone()).unwrap_or_default(),
            map: self.map.map(|m| m.info()),
            errors: self.errors,
            warnings: self.warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standardkarte_ist_gueltig() {
        let r = GameMap::load(DEFAULT_MAP);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let map = r.map.unwrap();
        assert_eq!((map.width, map.height), (64, 64));
        assert!(map.spawns.len() >= 2);
    }

    #[test]
    fn alle_karten_im_ordner_sind_gueltig() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/maps");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "txt") {
                let r = GameMap::load(&std::fs::read_to_string(&path).unwrap());
                assert!(r.errors.is_empty(), "{}: {:?}", path.display(), r.errors);
            }
        }
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
    fn fehler_werden_gesammelt_und_karte_bleibt_sichtbar() {
        let r = GameMap::load("0>>>0\n0x000\n");
        assert_eq!(r.errors.len(), 2);
        assert_eq!((r.errors[0].x, r.errors[0].y), (Some(1), Some(1)), "Zeichen zuerst");
        let map = r.map.expect("Karte trotzdem da");
        assert_eq!(map.tiles[1], TileKind::Ground, "kaputte Rampe wird Boden");
    }

    #[test]
    fn wasser_liegt_auf_umgebender_stufe() {
        let map = GameMap::parse("111\n1~1\n111\n").unwrap();
        assert_eq!(map.levels[4], 1);
        assert_eq!(map.corners[4], [12; 4]);
    }

    #[test]
    fn ressourcen_und_wald_blockiert() {
        let text = "spawn 0 0\nspawn 3 0\n[terrain]\n0000\n0000\n[resources]\n.WA.\n.WEF\n";
        let r = GameMap::load(text);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let map = r.map.unwrap();
        assert_eq!(map.resources[1], ResourceKind::Forest);
        assert_eq!(map.resources[2], ResourceKind::Farmland);
        assert_eq!(map.resources[6], ResourceKind::Iron);
        assert_eq!(map.resources[7], ResourceKind::Fertile);
        assert!(!map.nav.is_open(1, 0), "Wald ist nicht begehbar");
        assert!(!map.nav.is_open(2, 1), "Eisen auch nicht");
        assert!(map.nav.is_open(2, 0), "Acker schon");
        // Die Waldwand teilt die Karte: rechts ist abgeschnitten.
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);

        // Abholzen gibt das Feld wieder frei.
        let mut map = map;
        map.nav.set_blocked(1, 0, false);
        assert!(map.nav.can_step(0, 0, 1, 0));
    }

    #[test]
    fn ressourcen_nur_auf_boden() {
        let r = GameMap::load("[terrain]\n0~\n[resources]\n.W\n");
        assert!(r.errors.iter().any(|e| e.x == Some(1)));
    }
}
