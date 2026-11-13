//! Karten: werden aus einer Textdatei gelesen (siehe `maps/*.txt`).
//!
//! Maßstab: ein Feld ist etwa 3 × 3 m. Jedes Feld hat eine Höhenstufe 0–9
//! (eine Stufe ≈ 1,5 m) oder ist Wasser. Nachbarn, die sich um genau eine
//! Stufe unterscheiden, gehen als Schräge ineinander über und sind begehbar.
//! Bei zwei oder mehr Stufen Unterschied steht dazwischen eine Wand (Klippe).
//! Rampen muss man also nicht malen, sie entstehen aus den Höhen.
//!
//! Aufbau der Datei:
//! ```text
//! # Kommentar
//! spawn 10 10
//! [terrain]
//! 0001122333
//! ...
//! [resources]        (optional, gleiche Größe wie [terrain])
//! ..WW....
//! ```
//! Ohne Abschnittsüberschriften ist alles `[terrain]`.
//!
//! [`GameMap::load`] sammelt alle Fehler und liefert trotzdem eine Karte,
//! damit der Editor sie anzeigen kann.

use protocol::{MapCheck, MapIssue, ResourceKind, Text, TileKind};

use crate::nav::NavGrid;

/// Höhenschritte pro Stufe (für Eckhöhen). 12 lässt sich durch 2, 3 und 4
/// teilen, so sind die Mittelwerte an den Ecken immer ganze Schritte.
pub const STEPS_PER_LEVEL: u16 = 12;

/// Höchste Höhenstufe.
pub const MAX_LEVEL: u8 = 9;

/// Die Standardkarte, beim Kompilieren eingebettet (Rückfall, falls der
/// Kartenordner fehlt).
pub const DEFAULT_MAP: &str = include_str!("../maps/zwei-burgen.txt");

/// Größte erlaubte Kartenseite.
pub const MAX_SIZE: usize = 1024;

/// Eine geladene Karte mit allem, was der Server zur Laufzeit braucht.
pub struct GameMap {
    pub width: u32,
    pub height: u32,
    /// Boden, Schräge (Feld ist nicht eben) oder Wasser.
    pub tiles: Vec<TileKind>,
    pub resources: Vec<ResourceKind>,
    /// Höhenstufe pro Feld (bei Wasser die Stufe der Wasseroberfläche).
    pub levels: Vec<u8>,
    /// Vier Eckhöhen pro Feld: NW, NO, SO, SW (in Schritten).
    pub corners: Vec<[u16; 4]>,
    pub nav: NavGrid,
    /// Startfelder der Spieler.
    pub spawns: Vec<(u32, u32)>,
    /// Kommentarzeilen der Datei (für [`GameMap::to_text`]).
    pub comments: Vec<String>,
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

pub fn resource_char(r: ResourceKind) -> char {
    match r {
        ResourceKind::None => '.',
        ResourceKind::Forest => 'W',
        ResourceKind::Stone => 'S',
        ResourceKind::Iron => 'E',
        ResourceKind::Farmland => 'A',
        ResourceKind::Fertile => 'F',
    }
}

fn issue(x: i32, y: i32, text: Text) -> MapIssue {
    MapIssue { x: Some(x as u32), y: Some(y as u32), text }
}

fn general(text: Text) -> MapIssue {
    MapIssue { x: None, y: None, text }
}

/// Eckhöhen aller Felder aus den Stufen.
///
/// An jeder Gitterecke treffen bis zu vier Felder zusammen. Felder, die sich
/// dort über Kanten mit höchstens einer Stufe Unterschied verbinden, bilden
/// eine Gruppe und bekommen an dieser Ecke dieselbe Höhe: den Mittelwert der
/// Gruppe. So entstehen glatte Schrägen. Felder in verschiedenen Gruppen
/// haben verschiedene Höhen: dort steht eine Wand. Wasser ist immer flach.
///
/// Muss genau wie `computeCorners` im Client (`src/terrainmodel.ts`) rechnen.
pub fn compute_corners(width: usize, height: usize, levels: &[u8], water: &[bool]) -> Vec<[u16; 4]> {
    let n = width * height;
    let mut corners = vec![[0u16; 4]; n];
    let s = STEPS_PER_LEVEL;
    for i in 0..n {
        if water[i] {
            corners[i] = [levels[i] as u16 * s; 4];
        }
    }
    // Gitterecke (vx, vy): Felder NW (vx-1, vy-1), NO (vx, vy-1), SO (vx, vy), SW (vx-1, vy).
    // Index der Ecke, die die Gitterecke im jeweiligen Feld ist: NW-Feld -> SO-Ecke usw.
    const CELL: [(i32, i32); 4] = [(-1, -1), (0, -1), (0, 0), (-1, 0)];
    const CORNER_IN_CELL: [usize; 4] = [2, 3, 0, 1];
    // Kanten innerhalb des 2×2-Blocks (Paare von Positionen in CELL).
    const EDGES: [(usize, usize); 4] = [(0, 1), (1, 2), (2, 3), (3, 0)];
    for vy in 0..=height as i32 {
        for vx in 0..=width as i32 {
            let mut idx = [usize::MAX; 4];
            for (k, (dx, dy)) in CELL.iter().enumerate() {
                let (x, y) = (vx + dx, vy + dy);
                if x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height {
                    let i = y as usize * width + x as usize;
                    if !water[i] {
                        idx[k] = i;
                    }
                }
            }
            // Kleines Union-Find über vier Einträge.
            let mut group = [0usize, 1, 2, 3];
            let find = |g: &[usize; 4], mut a: usize| {
                while g[a] != a {
                    a = g[a];
                }
                a
            };
            for &(a, b) in &EDGES {
                if idx[a] == usize::MAX || idx[b] == usize::MAX {
                    continue;
                }
                if (levels[idx[a]] as i32 - levels[idx[b]] as i32).abs() <= 1 {
                    let (ra, rb) = (find(&group, a), find(&group, b));
                    group[ra.max(rb)] = ra.min(rb);
                }
            }
            for k in 0..4 {
                if idx[k] == usize::MAX {
                    continue;
                }
                let root = find(&group, k);
                let (mut sum, mut count) = (0u16, 0u16);
                for j in 0..4 {
                    if idx[j] != usize::MAX && find(&group, j) == root {
                        sum += levels[idx[j]] as u16;
                        count += 1;
                    }
                }
                corners[idx[k]][CORNER_IN_CELL[k]] = sum * s / count;
            }
        }
    }
    corners
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
            let args: Vec<String> = e.text.args.iter().map(|(k, v)| format!("{k}={v}")).collect();
            return Err(format!("{at}{} {}", e.text.key, args.join(" ")));
        }
        r.map.ok_or_else(|| "Karte ist leer".into())
    }

    /// Liest eine Karte und sammelt dabei alle Fehler und Hinweise.
    pub fn load(text: &str) -> LoadResult {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut terrain: Vec<&[u8]> = Vec::new();
        let mut res_rows: Vec<(usize, &[u8])> = Vec::new();
        let mut spawns = Vec::new();
        let mut comments = Vec::new();
        let mut in_resources = false;

        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            if let Some(c) = line.strip_prefix('#') {
                if terrain.is_empty() && !c.starts_with(" Startfelder") {
                    comments.push(c.to_string());
                }
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                match name {
                    "terrain" => in_resources = false,
                    "resources" => in_resources = true,
                    _ => errors.push(general(Text::new("map.unknown_section").arg("line", n + 1).arg("name", name))),
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("spawn ") {
                let nums: Vec<u32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                match nums[..] {
                    [x, y] => spawns.push((x, y)),
                    _ => errors.push(general(Text::new("map.spawn_args").arg("line", n + 1))),
                }
                continue;
            }
            if in_resources {
                res_rows.push((n + 1, line.as_bytes()));
            } else {
                terrain.push(line.as_bytes());
            }
        }

        let height = terrain.len();
        let width = terrain.first().map_or(0, |r| r.len());
        if width == 0 {
            errors.push(general(Text::new("map.empty")));
            return LoadResult { map: None, errors, warnings };
        }
        if width > MAX_SIZE || height > MAX_SIZE {
            errors.push(general(Text::new("map.too_big").arg("max", MAX_SIZE)));
            return LoadResult { map: None, errors, warnings };
        }
        if let Some(y) = terrain.iter().position(|r| r.len() != width) {
            errors.push(general(
                Text::new("map.row_length").arg("row", y + 1).arg("len", terrain[y].len()).arg("width", width),
            ));
            return LoadResult { map: None, errors, warnings };
        }

        let (w, h) = (width as i32, height as i32);
        let n = width * height;
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h;

        // 1. Stufen und Wasser.
        let mut levels = vec![0u8; n];
        let mut water = vec![false; n];
        let mut ramp_reported = false;
        for (y, row) in terrain.iter().enumerate() {
            for (x, &c) in row.iter().enumerate() {
                let i = y * width + x;
                match c {
                    b'0'..=b'9' => levels[i] = c - b'0',
                    b'~' => water[i] = true,
                    b'>' | b'<' | b'v' | b'^' => {
                        // Altes Format mit gemalten Rampen: einmal melden reicht.
                        if !ramp_reported {
                            errors.push(issue(x as i32, y as i32, Text::new("map.ramp_obsolete")));
                            ramp_reported = true;
                        }
                    }
                    _ => errors.push(issue(x as i32, y as i32, Text::new("map.unknown_char").arg("char", c as char))),
                }
            }
        }

        // 2. Wasser: jede zusammenhängende Wasserfläche liegt auf der
        //    niedrigsten Stufe des angrenzenden Bodens.
        let mut seen = vec![false; n];
        for start in 0..n {
            if !water[start] || seen[start] {
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
                    if water[j] {
                        if !seen[j] {
                            seen[j] = true;
                            body.push(j);
                            stack.push(j);
                        }
                    } else {
                        level = level.min(levels[j]);
                    }
                }
            }
            if level == u8::MAX {
                level = 0;
            }
            for i in body {
                levels[i] = level;
            }
        }

        // 3. Eckhöhen und Feldart.
        let corners = compute_corners(width, height, &levels, &water);
        let tiles: Vec<TileKind> = (0..n)
            .map(|i| {
                let c = corners[i];
                if water[i] {
                    TileKind::Water
                } else if c.iter().all(|&v| v == c[0]) {
                    TileKind::Ground
                } else {
                    TileKind::Ramp
                }
            })
            .collect();

        // 4. Ressourcen-Ebene.
        let mut resources = vec![ResourceKind::None; n];
        if !res_rows.is_empty() {
            if res_rows.len() != height {
                errors.push(general(Text::new("map.res_rows").arg("rows", res_rows.len()).arg("height", height)));
            }
            for (y, (line_no, row)) in res_rows.iter().enumerate().take(height) {
                if row.len() != width {
                    errors.push(general(
                        Text::new("map.res_row_length").arg("line", line_no).arg("len", row.len()).arg("width", width),
                    ));
                }
                for (x, &c) in row.iter().enumerate().take(width) {
                    let (xi, yi) = (x as i32, y as i32);
                    match resource_from_char(c as char) {
                        None => errors.push(issue(xi, yi, Text::new("map.unknown_resource").arg("char", c as char))),
                        Some(ResourceKind::None) => {}
                        Some(r) => {
                            let i = idx(xi, yi);
                            if water[i] {
                                errors.push(issue(xi, yi, Text::new("map.resource_on_water")));
                            } else {
                                resources[i] = r;
                            }
                        }
                    }
                }
            }
        }

        // 5. Startfelder.
        let mut valid_spawns = Vec::new();
        for &(x, y) in &spawns {
            let ok = (x as usize) < width
                && (y as usize) < height
                && !water[y as usize * width + x as usize]
                && !resource_blocks(resources[y as usize * width + x as usize]);
            if ok {
                valid_spawns.push((x, y));
            } else {
                errors.push(MapIssue {
                    x: ((x as usize) < width).then_some(x),
                    y: ((y as usize) < height).then_some(y),
                    text: Text::new("map.spawn_invalid").arg("x", x).arg("y", y),
                });
            }
        }
        if valid_spawns.len() < 2 {
            warnings.push(general(Text::new("map.few_spawns")));
        }

        let blocked: Vec<bool> = resources.iter().map(|&r| resource_blocks(r)).collect();
        let nav = NavGrid::build(width as u32, height as u32, &levels, &water, &blocked);
        let map = GameMap {
            width: width as u32,
            height: height as u32,
            tiles,
            resources,
            levels,
            corners,
            nav,
            spawns: valid_spawns,
            comments,
        };
        warnings.extend(map.reachability_warnings());
        LoadResult { map: Some(map), errors, warnings }
    }

    /// Begehbare Gebiete, die vom ersten Startfeld aus nicht erreichbar sind.
    /// Eine Meldung pro abgeschnittenem Gebiet (höchstens 50).
    fn reachability_warnings(&self) -> Vec<MapIssue> {
        let Some(&(sx, sy)) = self.spawns.first() else { return Vec::new() };
        let (w, h) = (self.width as i32, self.height as i32);
        let n = (self.width * self.height) as usize;
        let mut region = vec![u32::MAX; n];
        let mut regions = Vec::new(); // (Größe, Beispielfeld)
        let mut stack = Vec::new();
        let mut flood = |start: (i32, i32), id: u32, region: &mut Vec<u32>| {
            stack.clear();
            stack.push(start);
            region[(start.1 * w + start.0) as usize] = id;
            let mut size = 0;
            while let Some((x, y)) = stack.pop() {
                size += 1;
                for &(dx, dy) in &crate::nav::DIRS {
                    let (nx, ny) = (x + dx, y + dy);
                    if self.nav.can_step(x, y, dx, dy) && region[(ny * w + nx) as usize] == u32::MAX {
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
                if region[i] == u32::MAX && self.nav.is_open(x, y) {
                    let id = regions.len() as u32 + 1;
                    let size = flood((x, y), id, &mut region);
                    regions.push((size, (x, y)));
                }
            }
        }
        regions.sort_by_key(|r| std::cmp::Reverse(r.0));
        regions
            .into_iter()
            .take(50)
            .map(|(size, (x, y))| issue(x, y, Text::new("map.unreachable").arg("count", size)))
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

    /// Die Karte im Dateiformat, mit dem aktuellen Stand (z. B. abgeholzter Wald).
    /// Neue Clients laden sie über `/api/maps/current`.
    pub fn to_text(&self) -> String {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = String::with_capacity((w + 1) * h * 2 + 1024);
        for c in &self.comments {
            out.push('#');
            out.push_str(c);
            out.push('\n');
        }
        if !self.spawns.is_empty() {
            for (x, y) in &self.spawns {
                out.push_str(&format!("spawn {x} {y}\n"));
            }
        }
        out.push_str("[terrain]\n");
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                out.push(if self.tiles[i] == TileKind::Water { '~' } else { (b'0' + self.levels[i]) as char });
            }
            out.push('\n');
        }
        if self.resources.iter().any(|&r| r != ResourceKind::None) {
            out.push_str("[resources]\n");
            for y in 0..h {
                for x in 0..w {
                    out.push(resource_char(self.resources[y * w + x]));
                }
                out.push('\n');
            }
        }
        out
    }
}

impl LoadResult {
    pub fn into_check(self) -> MapCheck {
        MapCheck {
            spawns: self.map.as_ref().map(|m| m.spawns.clone()).unwrap_or_default(),
            width: self.map.as_ref().map_or(0, |m| m.width),
            height: self.map.as_ref().map_or(0, |m| m.height),
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
        assert!(map.width >= 128 && map.height >= 128);
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
    fn eine_stufe_unterschied_wird_schraege() {
        // Links Stufe 0, rechts Stufe 1: die Kante liegt auf halber Höhe.
        let map = GameMap::parse("0011\n0011\n").unwrap();
        assert_eq!(map.corners[1], [0, 6, 6, 0], "unteres Feld steigt zur Kante an");
        assert_eq!(map.corners[2], [6, 12, 12, 6], "oberes Feld fällt zur Kante ab");
        assert_eq!(map.tiles[0], TileKind::Ground);
        assert_eq!(map.tiles[1], TileKind::Ramp);
        assert!(map.nav.can_step(1, 0, 1, 0), "Schräge ist begehbar");
    }

    #[test]
    fn zwei_stufen_unterschied_wird_wand() {
        let map = GameMap::parse("0022\n0022\n").unwrap();
        assert_eq!(map.corners[1], [0; 4], "keine Schräge, Feld bleibt flach");
        assert_eq!(map.corners[2], [24; 4]);
        assert!(!map.nav.can_step(1, 0, 1, 0), "Wand ist nicht begehbar");
        let map = GameMap::parse("0122\n0122\n").unwrap();
        assert!(map.nav.can_step(0, 0, 1, 0) && map.nav.can_step(1, 0, 1, 0), "Treppe aus Einzelstufen geht");
    }

    #[test]
    fn alte_rampen_werden_gemeldet() {
        let r = GameMap::load("0>>>1\n");
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].text.key, "map.ramp_obsolete");
    }

    #[test]
    fn fehler_werden_gesammelt_und_karte_bleibt_sichtbar() {
        let r = GameMap::load("00x00\n0y000\n");
        assert_eq!(r.errors.len(), 2);
        assert!(r.map.is_some());
    }

    #[test]
    fn wasser_liegt_auf_umgebender_stufe() {
        let map = GameMap::parse("111\n1~1\n111\n").unwrap();
        assert_eq!(map.levels[4], 1);
        assert_eq!(map.corners[4], [12; 4]);
        assert_eq!(map.tiles[4], TileKind::Water);
        assert!(!map.nav.is_open(1, 1));
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
    fn text_hin_und_zurueck() {
        let map = GameMap::parse(DEFAULT_MAP).unwrap();
        let again = GameMap::parse(&map.to_text()).unwrap();
        assert_eq!(map.levels, again.levels);
        assert_eq!(map.resources, again.resources);
        assert_eq!(map.spawns, again.spawns);
        assert_eq!(map.tiles, again.tiles);
    }
}
