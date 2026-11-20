//! Wo der Computergegner was hinbaut.
//!
//! Um den Bergfried liegt ein (später gebauter) Mauerring: eckig mit einem Tor in der
//! Mitte jeder Seite, achteckig, oder nur an Engstellen der Karte (`CastleLayout`). Von Anfang an bleiben frei: der Ring selbst mit einem Feld Rand und
//! drei Felder breite Straßen vom Bergfried zu den Toren. Wohnhäuser, Lager und
//! Werkstätten kommen nach innen, Holzfäller, Steinbruch und Obstplantage nach außen.
//! Zwischen Gebäuden bleibt immer ein Feld Weg.

use std::collections::HashSet;

use protocol::{BuildingKind, BuildingState, CastleLayout, ResourceKind};

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
    /// Rechteck, aus dem der Grundriss entstand (bei der Engstelle: um die Linie).
    /// Nur zur Fehlersuche (`Debug`).
    #[allow(dead_code)]
    ring: Rect,
    pub mid: (i64, i64),
    #[allow(dead_code)]
    keep: Rect,
    /// Wie die Burg wirklich angelegt ist (Engstelle fällt auf eckig zurück, wenn es keine gibt).
    #[cfg_attr(not(test), allow(dead_code))]
    pub style: CastleLayout,
    /// Felder der Mauerlinie (mit den Feldern unter Toren), in Bauordnung.
    line: Vec<(i64, i64)>,
    on_line: HashSet<(i64, i64)>,
    /// Tore, das Haupttor zuerst.
    gates: Vec<GateSpot>,
    /// Türme (3x3): oben links.
    towers: Vec<(i64, i64)>,
    /// Innenraum (ohne Mauerlinie).
    inside: Reach,
    /// Je Tor die Mittellinie der Straße: vom Bergfried durchs Tor bis 3 Felder davor.
    streets: Vec<Vec<(i64, i64)>>,
}

/// Ein Tor im Ring: Art, oben links, Drehung.
pub type GateSpot = (BuildingKind, u32, u32, u8);

impl Layout {
    /// Grundriss in diesem Stil. Gerechnet wird nur mit Gelände und Vorkommen (nicht mit
    /// Gebäuden), damit er gleich bleibt, egal was schon steht.
    pub fn new(world: &World, keep: &BuildingState, gap: u32, main_gate: BuildingKind, style: CastleLayout) -> Layout {
        let square = Self::square(world, keep, gap, main_gate);
        match style {
            CastleLayout::Octagon => Self::octagon(world, keep, gap + 2, main_gate),
            CastleLayout::Narrows => Self::narrows(world, keep, &square).unwrap_or(square),
            _ => square,
        }
    }

    /// Rechteckiger Ring. Jede Seite liegt höchstens `gap` Felder vom Bergfried weg, näher
    /// dran, wenn dort sonst Wasser oder Klippen wären (etwa ein Burggraben: dann steht die
    /// Mauer an seinem Rand).
    fn square(world: &World, keep: &BuildingState, gap: u32, main_gate: BuildingKind) -> Layout {
        let (k, ring, mid) = Self::ring_rect(world, keep, gap);
        let main_side = main_side(keep, k);
        let line = rect_line(ring);
        let inside = Reach::filled(mid, |x, y| x > ring.x0 && x < ring.x1 && y > ring.y0 && y < ring.y1);
        let gates = side_gates(ring, mid, main_side, main_gate);
        let towers = vec![(ring.x0 - 1, ring.y0 - 1), (ring.x1 - 1, ring.y0 - 1), (ring.x0 - 1, ring.y1 - 1), (ring.x1 - 1, ring.y1 - 1)];
        let streets = side_streets(k, ring, mid, main_side);
        Self::assemble(ring, mid, k, CastleLayout::Square, line, gates, towers, inside, streets).fit_gates(world)
    }

    /// Ring mit abgeschrägten Ecken: etwas größer, Türme mitten auf den Schrägen.
    fn octagon(world: &World, keep: &BuildingState, gap: u32, main_gate: BuildingKind) -> Layout {
        let (k, ring, mid) = Self::ring_rect(world, keep, gap);
        let main_side = main_side(keep, k);
        // Schräge so lang, dass auf jeder Seite noch Platz fürs Tor (5 Felder) bleibt.
        let short = (ring.x1 - ring.x0).min(ring.y1 - ring.y0);
        let cut = ((short - 7) / 2).clamp(0, short / 3);
        let region = |x: i64, y: i64| {
            ring.contains(x, y)
                && (x - ring.x0) + (y - ring.y0) >= cut
                && (ring.x1 - x) + (y - ring.y0) >= cut
                && (x - ring.x0) + (ring.y1 - y) >= cut
                && (ring.x1 - x) + (ring.y1 - y) >= cut
        };
        let mut line = Vec::new();
        for y in ring.y0..=ring.y1 {
            for x in ring.x0..=ring.x1 {
                // Rand: im Achteck, aber ein Nachbar (4 Richtungen) draußen. Diagonal kommt
                // niemand durch, weil keiner Ecken abschneidet.
                if region(x, y) && [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| !region(x + dx, y + dy)) {
                    line.push((x, y));
                }
            }
        }
        let on: HashSet<(i64, i64)> = line.iter().copied().collect();
        let inside = Reach::filled(mid, |x, y| region(x, y) && !on.contains(&(x, y)));
        let gates = side_gates(ring, mid, main_side, main_gate);
        // Türme mitten auf den vier Schrägen.
        let h = cut / 2;
        let towers = if cut >= 4 {
            [(ring.x0 + h, ring.y0 + cut - h), (ring.x1 - h, ring.y0 + cut - h), (ring.x0 + h, ring.y1 - cut + h), (ring.x1 - h, ring.y1 - cut + h)]
                .iter()
                .map(|&(x, y)| (x - 1, y - 1))
                .collect()
        } else {
            Vec::new()
        };
        let streets = side_streets(k, ring, mid, main_side);
        Self::assemble(ring, mid, k, CastleLayout::Octagon, line, gates, towers, inside, streets).fit_gates(world)
    }

    /// Mauer nur an Engstellen: Wasser, Wald, Fels und Hänge sind die halbe Mauer. Gesucht
    /// wird die Entfernung vom Bergfried (in Schritten), bei der die wenigsten Felder nach
    /// draußen führen; nur diese bekommen Mauer und Tore. Lohnt sich das nicht deutlich
    /// gegenüber dem eckigen Ring, `None`.
    fn narrows(world: &World, keep: &BuildingState, square: &Layout) -> Option<Layout> {
        let k = Rect::of(keep);
        let start = keep.entrance.map(|(x, y)| (x as i64, y as i64))?;
        let field = Field::new(world, start);
        let mid = square.mid;
        // Ganz von Natur umschlossen (Insel, Wald ringsum): keine Mauer nötig.
        if field.outer.is_empty() {
            let inside = Reach::filled(mid, |x, y| field.dist(x, y).is_some());
            let ring = field.bounds();
            return Some(Self::assemble(ring, mid, k, CastleLayout::Narrows, Vec::new(), Vec::new(), Vec::new(), inside, Vec::new()));
        }
        let mut options: Vec<(f32, u16)> = (NARROWS_MIN..=NARROWS_MAX)
            .map(|d| (field.cut(d).0.len() as f32 - 0.4 * d as f32, d))
            .collect();
        options.sort_by(|a, b| a.0.total_cmp(&b.0));
        let limit = square.line.len() as f32 * NARROWS_SHARE;
        for &(_, d) in &options {
            let (line, outside) = field.cut(d);
            if line.len() as f32 > limit {
                break;
            }
            let gates = field.gates(world, &line, &outside, d);
            if gates.is_empty() {
                continue;
            }
            let on: HashSet<(i64, i64)> = line.iter().copied().collect();
            let inside = Reach::filled(mid, |x, y| field.dist(x, y).is_some() && !outside.contains(&(x, y)) && !on.contains(&(x, y)));
            let streets = gates.iter().map(|g| field.street(world, g)).collect();
            let ring = field.bounds_of(&line);
            return Some(Self::assemble(ring, mid, k, CastleLayout::Narrows, line, gates, Vec::new(), inside, streets).fit_gates(world));
        }
        None
    }

    /// Rechteck des eckigen Rings (an Wasser und Klippen angepasst).
    fn ring_rect(world: &World, keep: &BuildingState, gap: u32) -> (Rect, Rect, (i64, i64)) {
        let k = Rect::of(keep);
        let gap = (gap as i64).max(MIN_GAP);
        let start = keep.entrance.map_or((k.x0, k.y1 + 1), |(x, y)| (x as i64, y as i64));
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
        (k, ring, mid)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        ring: Rect,
        mid: (i64, i64),
        keep: Rect,
        style: CastleLayout,
        line: Vec<(i64, i64)>,
        gates: Vec<GateSpot>,
        towers: Vec<(i64, i64)>,
        inside: Reach,
        streets: Vec<Vec<(i64, i64)>>,
    ) -> Layout {
        let on_line = line.iter().copied().collect();
        Layout { ring, mid, keep, style, line, on_line, gates, towers, inside, streets }
    }

    /// Tore dürfen nicht auf Schrägen oder Wasser stehen: entlang der Mauer verschieben, bis
    /// es eben ist (die Straße biegt dann kurz vor dem Tor ab), sonst weglassen (dort wird
    /// Mauer). Eins bleibt mindestens, wenn es irgendwo geht.
    fn fit_gates(mut self, world: &World) -> Layout {
        let w = world.map.width as i64;
        let h = world.map.height as i64;
        let flat = |x: i64, y: i64| x >= 0 && y >= 0 && x < w && y < h && world.map.tiles[(y * w + x) as usize] == protocol::TileKind::Ground;
        let mut gates = Vec::new();
        let mut streets = Vec::new();
        for (i, g) in self.gates.iter().enumerate() {
            let along: (i64, i64) = if g.3 % 2 == 0 { (1, 0) } else { (0, 1) };
            let fits = |g: &GateSpot| {
                let r = Self::gate_rect(world, g);
                let tiles: Vec<(i64, i64)> = (r.y0..=r.y1).flat_map(|y| (r.x0..=r.x1).map(move |x| (x, y))).collect();
                tiles.iter().all(|&(x, y)| flat(x, y)) && tiles.iter().filter(|t| self.on_line.contains(t)).count() as i64 >= (r.x1 - r.x0 + 1).max(r.y1 - r.y0 + 1)
            };
            let moved = (0..=8i64).flat_map(|d| [d, -d]).find_map(|d| {
                let x = g.1 as i64 + along.0 * d;
                let y = g.2 as i64 + along.1 * d;
                let c = (g.0, x.max(0) as u32, y.max(0) as u32, g.3);
                (x >= 0 && y >= 0 && fits(&c)).then_some((d, c))
            });
            let Some((d, c)) = moved else { continue };
            let street = self.streets.get(i).cloned().unwrap_or_default();
            streets.push(if d == 0 { street } else { self.bend_street(world, &street, &c) });
            gates.push(c);
        }
        self.gates = gates;
        self.streets = streets;
        self
    }

    /// Straße zum verschobenen Tor: bis vor die Mauer wie bisher, dann seitlich zum
    /// Durchgang, hindurch und 3 Felder hinaus.
    fn bend_street(&self, world: &World, street: &[(i64, i64)], g: &GateSpot) -> Vec<(i64, i64)> {
        let r = Self::gate_rect(world, g);
        let pass = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        let Some(cross) = street.iter().position(|t| self.on_line.contains(t) || Self::gate_rect(world, g).contains(t.0, t.1)) else {
            return street.to_vec();
        };
        let mut out: Vec<(i64, i64)> = street[..cross.saturating_sub(1)].to_vec();
        let mut at = *out.last().unwrap_or(&street[0]);
        // Quer zur Durchgangsrichtung bis zur Flucht des Durchgangs.
        let vertical = g.3.is_multiple_of(2);
        while (vertical && at.0 != pass.0) || (!vertical && at.1 != pass.1) {
            at = if vertical { (at.0 + (pass.0 - at.0).signum(), at.1) } else { (at.0, at.1 + (pass.1 - at.1).signum()) };
            out.push(at);
        }
        let dir = if vertical { (0, (pass.1 - at.1).signum()) } else { ((pass.0 - at.0).signum(), 0) };
        let mut t = at;
        for _ in 0..(r.x1 - r.x0).max(r.y1 - r.y0) + 5 {
            t = (t.0 + dir.0, t.1 + dir.1);
            out.push(t);
        }
        out
    }

    /// Liegt das Feld im Innenraum (nicht auf der Mauerlinie)?
    pub fn inside(&self, x: i64, y: i64) -> bool {
        self.inside.at(x, y)
    }

    /// Im Innenraum oder höchstens `margin` Felder von der Mauerlinie.
    fn within(&self, x: i64, y: i64, margin: i64) -> bool {
        self.inside(x, y) || (-margin..=margin).any(|dy| (-margin..=margin).any(|dx| self.on_line.contains(&(x + dx, y + dy))))
    }

    /// Felder, auf denen in dieser Zone ein Gebäude (ohne Mauern) stehen darf: nicht auf
    /// Mauerlinie, Toren, Türmen und Straßen, mit einem Feld Abstand zu allen Gebäuden.
    fn free_tiles(&self, world: &World, zone: Zone) -> Vec<bool> {
        let (w, h) = (world.map.width as i64, world.map.height as i64);
        let mut free = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                free[(y * w + x) as usize] = match zone {
                    Zone::Inside => self.inside(x, y),
                    Zone::Outside => !self.inside(x, y),
                };
            }
        }
        let mut clear = |r: Rect| {
            for y in r.y0.max(0)..=r.y1.min(h - 1) {
                for x in r.x0.max(0)..=r.x1.min(w - 1) {
                    free[(y * w + x) as usize] = false;
                }
            }
        };
        // Mauerlinie mit Rand: innen ein Feld, außen zwei (dort laufen Wege entlang).
        for &(x, y) in &self.line {
            clear(Rect { x0: x, y0: y, x1: x, y1: y }.grow(if zone == Zone::Inside { 1 } else { 2 }));
        }
        for g in &self.gates {
            clear(Self::gate_rect(world, g).grow(2));
        }
        for &(x, y) in &self.towers {
            clear(Rect { x0: x, y0: y, x1: x + 2, y1: y + 2 }.grow(1));
        }
        // Straßen drei Felder breit.
        for street in &self.streets {
            for &(x, y) in street {
                clear(Rect { x0: x, y0: y, x1: x, y1: y }.grow(1));
            }
        }
        for b in &world.buildings {
            // Wege: nur das Feld selbst ist belegt, daneben darf direkt gebaut werden.
            let road = world.economy.building(b.kind).is_some_and(|d| d.is_road());
            clear(Rect::of(b).grow(if road { 0 } else { 1 }));
        }
        free
    }

    /// Die Felder der Mauerlinie in Bauordnung (auch unter den Toren).
    pub fn ring_tiles(&self) -> Vec<(i64, i64)> {
        self.line.clone()
    }

    /// Wo die Tore hinkommen, das Haupttor zuerst.
    pub fn gates(&self) -> Vec<GateSpot> {
        self.gates.clone()
    }

    /// Fläche eines Tores (gedreht).
    fn gate_rect(world: &World, &(kind, x, y, rot): &GateSpot) -> Rect {
        let (w, h) = world.economy.building(kind).map_or((3, 1), |d| (d.w as i64, d.h as i64));
        let (w, h) = if rot % 2 == 1 { (h, w) } else { (w, h) };
        Rect { x0: x as i64, y0: y as i64, x1: x as i64 + w - 1, y1: y as i64 + h - 1 }
    }

    /// Gehört dieses Feld zu einem Tor (dort keine Mauer setzen)?
    pub fn is_gate_tile(&self, world: &World, x: i64, y: i64) -> bool {
        self.gates.iter().any(|g| Self::gate_rect(world, g).contains(x, y))
    }

    /// Türme (3x3): oben links.
    pub fn corner_towers(&self) -> Vec<(i64, i64)> {
        self.towers.clone()
    }

    /// Liegt das Feld auf der Mauerlinie?
    pub fn on_ring(&self, x: i64, y: i64) -> bool {
        self.on_line.contains(&(x, y))
    }

    /// Wegfelder in der Mitte der Straßen: vom Bergfried durch das Tor bis 3 Felder davor.
    /// `all`: zu allen Toren, sonst nur zum Haupttor. Das Haupttor zuerst.
    pub fn street_tiles(&self, all: bool) -> Vec<(i64, i64)> {
        let n = if all { self.streets.len() } else { self.streets.len().min(1) };
        self.streets.iter().take(n).flatten().copied().collect()
    }

    /// Feld vor einem Tor auf der Außenseite (dort beginnen Wege nach draußen).
    pub fn outside_of(&self, world: &World, gate: &GateSpot) -> (i64, i64) {
        let g = Self::gate_rect(world, gate);
        let (mx, my) = ((g.x0 + g.x1) / 2, (g.y0 + g.y1) / 2);
        let candidates = match gate.3 % 2 {
            0 => [(mx, g.y1 + 1), (mx, g.y0 - 1)],
            _ => [(g.x0 - 1, my), (g.x1 + 1, my)],
        };
        // Die Seite, die nicht im Innenraum liegt.
        candidates.into_iter().find(|&(x, y)| !self.inside(x, y)).unwrap_or(candidates[0])
    }
}

/// So nah (in Schritten vom Bergfried) darf die Mauer an einer Engstelle höchstens bzw.
/// muss sie mindestens liegen.
const NARROWS_MIN: u16 = 12;
const NARROWS_MAX: u16 = 42;
/// Engstelle nur, wenn sie höchstens diesen Anteil der Felder des eckigen Rings braucht.
const NARROWS_SHARE: f32 = 0.6;

/// Seite, zu der der Eingang des Bergfrieds schaut: 0 Süd, 1 Nord, 2 West, 3 Ost.
fn main_side(keep: &BuildingState, k: Rect) -> usize {
    match keep.entrance {
        Some((_, y)) if y as i64 > k.y1 => 0,
        Some((_, y)) if (y as i64) < k.y0 => 1,
        Some((x, _)) if (x as i64) < k.x0 => 2,
        Some(_) => 3,
        None => 0,
    }
}

/// Felder am Rand eines Rechtecks, Seite für Seite.
fn rect_line(r: Rect) -> Vec<(i64, i64)> {
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

/// Tore in der Mitte jeder Seite, das Haupttor (Seite des Bergfried-Eingangs) zuerst.
/// Das Torhaus ist 5 breit, 2 tief und ragt nach außen.
fn side_gates(r: Rect, (mx, my): (i64, i64), main_side: usize, main_gate: BuildingKind) -> Vec<GateSpot> {
    let p = |v: i64| v.max(0) as u32;
    let spot = |side: usize| -> GateSpot {
        let big = side == main_side && main_gate == BuildingKind::Gatehouse;
        let kind = if side == main_side { main_gate } else { BuildingKind::Gate };
        let (half, out) = if big { (2, 1) } else { (1, 0) };
        match side {
            0 => (kind, p(mx - half), p(r.y1), 0),
            1 => (kind, p(mx - half), p(r.y0 - out), 2),
            2 => (kind, p(r.x0 - out), p(my - half), 1),
            _ => (kind, p(r.x1), p(my - half), 3),
        }
    };
    let mut sides = vec![main_side];
    sides.extend((0..4).filter(|&s| s != main_side));
    sides.into_iter().map(spot).collect()
}

/// Gerade Straßen vom Bergfried zu den Toren in der Mitte der Seiten, Haupttor zuerst.
fn side_streets(k: Rect, r: Rect, (mx, my): (i64, i64), main_side: usize) -> Vec<Vec<(i64, i64)>> {
    let mut sides = vec![main_side];
    sides.extend((0..4).filter(|&s| s != main_side));
    sides
        .into_iter()
        .map(|side| match side {
            0 => (k.y1 + 1..=r.y1 + 3).map(|y| (mx, y)).collect(),
            1 => (r.y0 - 3..k.y0).rev().map(|y| (mx, y)).collect(),
            2 => (r.x0 - 3..k.x0).rev().map(|x| (x, my)).collect(),
            _ => (k.x1 + 1..=r.x1 + 3).map(|x| (x, my)).collect(),
        })
        .collect()
}

/// Schritte vom Bergfried über Gelände und Vorkommen (für die Engstelle). Wald, Stein und
/// Eisen sperren wie Wasser und Klippen. Erlaubt mehr diagonale Schritte als die Einheiten
/// (auch um Ecken), damit die gefundene Linie sicher dicht ist.
struct Field {
    x0: i64,
    y0: i64,
    size: i64,
    dist: Vec<u16>,
    came: Vec<u32>,
    /// Felder knapp hinter der größten Entfernung: dort ist „draußen“.
    outer: Vec<(i64, i64)>,
    passable: Vec<bool>,
}

const FAR: u16 = u16::MAX;

impl Field {
    fn new(world: &World, start: (i64, i64)) -> Field {
        let map = &world.map;
        let r = NARROWS_MAX as i64 + 4 + 8;
        let size = 2 * r + 1;
        let (x0, y0) = (start.0 - r, start.1 - r);
        let (w, h) = (map.width as i64, map.height as i64);
        let mut passable = vec![false; (size * size) as usize];
        for ly in 0..size {
            for lx in 0..size {
                let (x, y) = (x0 + lx, y0 + ly);
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let i = (y * w + x) as usize;
                let res = map.resources[i];
                passable[(ly * size + lx) as usize] = map.tiles[i] != protocol::TileKind::Water
                    && !matches!(res, ResourceKind::Forest | ResourceKind::Stone | ResourceKind::Iron);
            }
        }
        let mut f = Field { x0, y0, size, dist: vec![FAR; (size * size) as usize], came: vec![u32::MAX; (size * size) as usize], outer: Vec::new(), passable };
        let Some(s) = f.index(start.0, start.1) else { return f };
        f.dist[s] = 0;
        f.came[s] = s as u32;
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some((x, y)) = queue.pop_front() {
            let d = f.dist[f.index(x, y).unwrap()];
            for (dx, dy) in DIRS8 {
                let (nx, ny) = (x + dx, y + dy);
                let Some(n) = f.index(nx, ny) else { continue };
                if f.dist[n] != FAR || !f.step(world, (x, y), (nx, ny)) {
                    continue;
                }
                f.dist[n] = d + 1;
                f.came[n] = f.index(x, y).unwrap() as u32;
                if d + 1 > NARROWS_MAX + 1 {
                    f.outer.push((nx, ny));
                } else {
                    queue.push_back((nx, ny));
                }
            }
        }
        f
    }

    fn index(&self, x: i64, y: i64) -> Option<usize> {
        let (lx, ly) = (x - self.x0, y - self.y0);
        (lx >= 0 && ly >= 0 && lx < self.size && ly < self.size).then(|| (ly * self.size + lx) as usize)
    }

    fn dist(&self, x: i64, y: i64) -> Option<u16> {
        self.index(x, y).map(|i| self.dist[i]).filter(|&d| d != FAR)
    }

    fn open(&self, x: i64, y: i64) -> bool {
        self.index(x, y).is_some_and(|i| self.passable[i])
    }

    fn step(&self, world: &World, a: (i64, i64), b: (i64, i64)) -> bool {
        if !self.open(a.0, a.1) || !self.open(b.0, b.1) {
            return false;
        }
        let w = world.map.width as i64;
        let la = world.map.levels[(a.1 * w + a.0) as usize];
        let lb = world.map.levels[(b.1 * w + b.0) as usize];
        la.abs_diff(lb) <= 1
    }

    /// Mauerlinie bei Entfernung `d`: die Felder mit genau dieser Entfernung, von denen es
    /// nach draußen weitergeht (Sackgassen bleiben offen). Dazu „draußen“.
    fn cut(&self, d: u16) -> (Vec<(i64, i64)>, TileSet) {
        // Von draußen nach innen fluten, nur über Felder weiter als `d`.
        let mut outside: HashSet<(i64, i64)> = self.outer.iter().copied().collect();
        let mut queue: std::collections::VecDeque<(i64, i64)> = self.outer.iter().copied().collect();
        while let Some((x, y)) = queue.pop_front() {
            for (dx, dy) in DIRS8 {
                let n = (x + dx, y + dy);
                if self.dist(n.0, n.1).is_some_and(|nd| nd > d) && outside.insert(n) {
                    queue.push_back(n);
                }
            }
        }
        let mut line = Vec::new();
        for ly in 0..self.size {
            for lx in 0..self.size {
                let (x, y) = (self.x0 + lx, self.y0 + ly);
                if self.dist(x, y) == Some(d) && DIRS8.iter().any(|&(dx, dy)| outside.contains(&(x + dx, y + dy))) {
                    line.push((x, y));
                }
            }
        }
        (line, outside)
    }

    /// Je zusammenhängendem Stück der Linie ein Tor, wo drei Felder in einer Reihe liegen
    /// und davor draußen, dahinter drinnen ist (möglichst in der Mitte des Stücks, ebenes
    /// Gelände). Größtes Stück zuerst.
    fn gates(&self, world: &World, line: &[(i64, i64)], outside: &HashSet<(i64, i64)>, d: u16) -> Vec<GateSpot> {
        let on: HashSet<(i64, i64)> = line.iter().copied().collect();
        let mut seen: HashSet<(i64, i64)> = HashSet::new();
        let mut segments: Vec<Vec<(i64, i64)>> = Vec::new();
        for &t in line {
            if !seen.insert(t) {
                continue;
            }
            let mut seg = vec![t];
            let mut i = 0;
            while i < seg.len() {
                let (x, y) = seg[i];
                for (dx, dy) in DIRS8 {
                    let n = (x + dx, y + dy);
                    if on.contains(&n) && seen.insert(n) {
                        seg.push(n);
                    }
                }
                i += 1;
            }
            segments.push(seg);
        }
        segments.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let w = world.map.width as i64;
        let level = |x: i64, y: i64| world.map.levels[(y * w + x) as usize];
        let inner = |x: i64, y: i64| self.dist(x, y).is_some_and(|nd| nd < d);
        let mut gates = Vec::new();
        for seg in &segments {
            let cx = seg.iter().map(|t| t.0).sum::<i64>() as f32 / seg.len() as f32;
            let cy = seg.iter().map(|t| t.1).sum::<i64>() as f32 / seg.len() as f32;
            let mut best: Option<(f32, GateSpot)> = None;
            for &(x, y) in seg {
                let ground = |t: (i64, i64)| world.map.tiles[(t.1 * w + t.0) as usize] == protocol::TileKind::Ground;
                let flat = |a: (i64, i64), b: (i64, i64)| {
                    level(a.0, a.1) == level(x, y) && level(b.0, b.1) == level(x, y) && ground(a) && ground(b) && ground((x, y))
                };
                // Waagrecht: Durchgang nach oben/unten.
                let across_v = (inner(x, y - 1) && outside.contains(&(x, y + 1))) || (inner(x, y + 1) && outside.contains(&(x, y - 1)));
                if on.contains(&(x - 1, y)) && on.contains(&(x + 1, y)) && across_v && flat((x - 1, y), (x + 1, y)) && x >= 1 {
                    let score = (x as f32 - cx).hypot(y as f32 - cy);
                    if best.is_none_or(|b| score < b.0) {
                        best = Some((score, (BuildingKind::Gate, (x - 1) as u32, y as u32, 0)));
                    }
                }
                let across_h = (inner(x - 1, y) && outside.contains(&(x + 1, y))) || (inner(x + 1, y) && outside.contains(&(x - 1, y)));
                if on.contains(&(x, y - 1)) && on.contains(&(x, y + 1)) && across_h && flat((x, y - 1), (x, y + 1)) && y >= 1 {
                    let score = (x as f32 - cx).hypot(y as f32 - cy);
                    if best.is_none_or(|b| score < b.0) {
                        best = Some((score, (BuildingKind::Gate, x as u32, (y - 1) as u32, 1)));
                    }
                }
            }
            if let Some((_, g)) = best {
                gates.push(g);
            }
        }
        gates
    }

    /// Straße zum Tor: rückwärts entlang der Schritte vom Durchgang zum Bergfried, dann durchs
    /// Tor und 3 Felder hinaus.
    fn street(&self, world: &World, g: &GateSpot) -> Vec<(i64, i64)> {
        let r = Layout::gate_rect(world, g);
        let (px, py) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        let (a, b) = if g.3.is_multiple_of(2) { ((px, py - 1), (px, py + 1)) } else { ((px - 1, py), (px + 1, py)) };
        let d = |t: (i64, i64)| self.dist(t.0, t.1).unwrap_or(FAR);
        let (inner, outer) = if d(a) < d(b) { (a, b) } else { (b, a) };
        let mut path = Vec::new();
        let mut at = self.index(inner.0, inner.1);
        while let Some(i) = at {
            let t = (self.x0 + i as i64 % self.size, self.y0 + i as i64 / self.size);
            path.push(t);
            let prev = self.came[i];
            at = (prev != u32::MAX && prev as usize != i).then_some(prev as usize);
        }
        path.reverse();
        path.push((px, py));
        let dir = (outer.0 - px, outer.1 - py);
        path.extend((1..=3).map(|s| (px + dir.0 * s, py + dir.1 * s)));
        path
    }

    fn bounds(&self) -> Rect {
        let tiles: Vec<(i64, i64)> = (0..self.size * self.size)
            .filter(|&i| self.dist[i as usize] != FAR)
            .map(|i| (self.x0 + i % self.size, self.y0 + i / self.size))
            .collect();
        self.bounds_of(&tiles)
    }

    fn bounds_of(&self, tiles: &[(i64, i64)]) -> Rect {
        let mut r = Rect { x0: i64::MAX, y0: i64::MAX, x1: i64::MIN, y1: i64::MIN };
        for &(x, y) in tiles {
            r = Rect { x0: r.x0.min(x), y0: r.y0.min(y), x1: r.x1.max(x), y1: r.y1.max(y) };
        }
        r
    }
}

/// Menge von Feldern.
type TileSet = HashSet<(i64, i64)>;

const DIRS8: [(i64, i64); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

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
                // Nur Vorkommen, an die seine Leute herankommen; gemessen am Laufweg, nicht an
                // der Luftlinie (ein Waldstreifen dazwischen ist ein Umweg).
                let Some(walk) = [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().filter_map(|&(nx, ny)| reach.walk(x + nx, y + ny)).min() else {
                    continue;
                };
                // Felder im Ring zählen weniger: dort baut er keine Abbaubetriebe hin.
                let score = walk as i64 + if layout.within(x, y, 2) { 12 } else { 0 };
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
    // Schon so viele, wie erlaubt: nirgends ein Platz, ohne die ganze Gegend abzusuchen.
    if world.check_limit(player, kind).is_err() {
        return None;
    }
    let mut want: Vec<ResourceKind> = def.gathers.clone();
    if let Some(p) = &def.placement {
        want.extend(p.resources.iter().copied());
    }
    // Was ein Vorkommen braucht (Holzfäller, Steinbruch, Acker), kommt dorthin, also nach
    // außen; alles andere möglichst nah an den Bergfried, in den Ring.
    // Soll es neben etwas Bestimmtes, dann auf dieselbe Seite des Rings.
    let zone = match near {
        Some((x, y)) if layout.inside(x, y) => Zone::Inside,
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
        // Summenfeld der belegten Felder: ob ein Rechteck frei ist, kostet dann vier Zugriffe
        // statt eines je Feld (die Suche prüft hunderttausende Rechtecke).
        let blocked = BlockedSums::new(&free, mw, mh);
        let fits = |r: &Rect| r.x0 >= 0 && r.y0 >= 0 && r.x1 < mw && r.y1 < mh && blocked.none_in(r);
        // Bester Platz: nah am Ziel, aber gemessen am Laufweg vom Bergfried. Was man nur über
        // einen Umweg erreicht (etwa hinter einem Waldstreifen), zählt um den Umweg weiter weg.
        let (sx, sy) = (start.0 as i64, start.1 as i64);
        let mut best: Option<(i64, (u32, u32, u8))> = None;
        for r in 0..=SEARCH_RADIUS {
            // Ein Platz im Ring r kostet mindestens r: weiter außen wird es nicht besser.
            if best.is_some_and(|(score, _)| score <= r) {
                break;
            }
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
                        let (ex, ey) = entrance_of(&rect, rotation);
                        let Some(walk) = reach.walk(ex as i64, ey as i64) else { continue };
                        let straight = (ex as i64 - sx).abs().max((ey as i64 - sy).abs());
                        let score = r + (walk as i64 - straight).max(0);
                        if best.is_some_and(|(b, _)| score >= b) {
                            continue;
                        }
                        if world.check_placement(player, kind, (x as u32, y as u32), rotation, 1).is_err() {
                            continue;
                        }
                        best = Some((score, (x as u32, y as u32, rotation)));
                    }
                }
            }
        }
        if let Some((_, site)) = best {
            return Some(site);
        }
    }
    None
}

/// Wie viele belegte Felder links oberhalb liegen (Summenfeld, (w+1)×(h+1)).
struct BlockedSums {
    w: i64,
    sums: Vec<u32>,
}

impl BlockedSums {
    fn new(free: &[bool], w: i64, h: i64) -> BlockedSums {
        let stride = w + 1;
        let mut sums = vec![0u32; (stride * (h + 1)) as usize];
        for y in 0..h {
            let mut row = 0u32;
            for x in 0..w {
                row += u32::from(!free[(y * w + x) as usize]);
                sums[((y + 1) * stride + x + 1) as usize] = sums[(y * stride + x + 1) as usize] + row;
            }
        }
        BlockedSums { w, sums }
    }

    /// Kein belegtes Feld im Rechteck (das ganz auf der Karte liegt).
    fn none_in(&self, r: &Rect) -> bool {
        let stride = self.w + 1;
        let at = |x: i64, y: i64| self.sums[(y * stride + x) as usize];
        at(r.x1 + 1, r.y1 + 1) + at(r.x0, r.y0) == at(r.x0, r.y1 + 1) + at(r.x1 + 1, r.y0)
    }
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
#[derive(Debug, Clone)]
struct Reach {
    x0: i64,
    y0: i64,
    size: i64,
    seen: Vec<bool>,
    /// Schritte vom Start (nur bei `flood`, sonst 0).
    steps: Vec<u16>,
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
        let mut reach = Reach { x0, y0, size, seen: vec![false; (size * size) as usize], steps: vec![0; (size * size) as usize] };
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
                    let d = reach.walk(x, y).unwrap_or(0).saturating_add(1);
                    reach.mark(nx, ny);
                    if let Some(i) = reach.index(nx, ny) {
                        reach.steps[i] = d;
                    }
                    queue.push_back((nx, ny));
                }
            }
        }
        reach
    }

    /// Alle Felder um `center`, für die `inside` gilt.
    fn filled(center: (i64, i64), inside: impl Fn(i64, i64) -> bool) -> Reach {
        let size = 2 * REACH_RADIUS + 1;
        let (x0, y0) = (center.0 - REACH_RADIUS, center.1 - REACH_RADIUS);
        let seen = (0..size * size).map(|i| inside(x0 + i % size, y0 + i / size)).collect();
        Reach { x0, y0, size, seen, steps: vec![0; (size * size) as usize] }
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

    /// Laufweg vom Start in Schritten, `None` = nicht erreichbar.
    fn walk(&self, x: i64, y: i64) -> Option<u16> {
        self.index(x, y).filter(|&i| self.seen[i]).map(|i| self.steps[i])
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

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::{economy::Economy, map::GameMap};

    /// Jeden Grundriss auf jedem Startplatz ganz bauen (ohne Kosten): danach muss die Burg
    /// geschlossen sein. Prüft, dass die Linien wirklich dicht sind.
    /// Hinter einem Waldstreifen liegt ein Platz zwar nah an der Luftlinie, aber der Weg
    /// führt außen herum: dann lieber diesseits des Waldes bauen.
    #[test]
    fn baut_nach_laufweg_nicht_nach_luftlinie() {
        let (w, h) = (100usize, 100usize);
        let terrain: Vec<String> = (0..h).map(|_| "0".repeat(w)).collect();
        // Wald in den Spalten 62..65, nur ganz unten offen.
        let resources: Vec<String> = (0..h)
            .map(|y| (0..w).map(|x| if (62..65).contains(&x) && y < 92 { 'W' } else { '.' }).collect())
            .collect();
        let text = format!("spawn 40 40\n[terrain]\n{}\n[resources]\n{}\n", terrain.join("\n"), resources.join("\n"));
        let map = GameMap::parse(&text).unwrap();
        let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_build_radius();
        let mut world = World::new(map, economy);
        world.spawn_player_at(1, (40, 40));
        world.give_everything_for_tests(1);
        let keep = world.buildings.iter().find(|b| b.owner == 1 && b.kind == BuildingKind::Keep).unwrap().clone();
        let layout = Layout::new(&world, &keep, 12, BuildingKind::Gate, CastleLayout::Square);
        // Gewünscht: gleich hinter dem Wald.
        let (x, _, _) = find_site(&world, 1, BuildingKind::House, &layout, Some((67, 40))).unwrap();
        assert!(x < 62, "Haus hinter dem Wald bei x = {x}");
    }

    fn check_map(text: &str, players: usize) -> Vec<String> {
        let mut report = Vec::new();
        for style in [CastleLayout::Square, CastleLayout::Octagon, CastleLayout::Narrows] {
            let map = GameMap::parse(text).unwrap();
            let spawns = map.spawns.clone();
            let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people().without_build_radius();
            let mut world = World::new(map, economy);
            for (i, &s) in spawns.iter().take(players).enumerate() {
                world.spawn_player_at(i as u32 + 1, s);
            }
            for p in 1..=players as u32 {
                world.give_everything_for_tests(p);
                let keep = world.buildings.iter().find(|b| b.owner == p && b.kind == BuildingKind::Keep).unwrap().clone();
                let layout = Layout::new(&world, &keep, 12, BuildingKind::Gatehouse, style);
                let mut failed = 0;
                for g in layout.gates() {
                    if let Err(e) = world.place_building_rotated(p, g.0, g.1, g.2, g.3, 1) {
                        println!("{style:?} P{p} Tor {g:?}: {} {:?}", e.key, e.args);
                        failed += 1;
                    }
                }
                for (x, y) in layout.ring_tiles() {
                    if x < 0 || y < 0 || layout.is_gate_tile(&world, x, y) {
                        continue;
                    }
                    if world.place_building_rotated(p, BuildingKind::Wall, x as u32, y as u32, 0, 1).is_err() {
                        failed += 0; // Wasser, Klippe, Wald: dort braucht es keine Mauer.
                    }
                }
                let st = world.castle_status(p);
                report.push(format!(
                    "{style:?} P{p} -> {:?}: {} Mauerfelder, {} Tore ({failed} nicht baubar), geschlossen {} Lücke {:?}",
                    layout.style,
                    layout.ring_tiles().len(),
                    layout.gates().len(),
                    st.closed,
                    st.gap
                ));
                assert!(st.closed, "{}", report.last().unwrap());
            }
        }
        report
    }

    #[test]
    fn grundrisse_sind_dicht_auf_zwei_burgen() {
        let r = check_map(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/zwei-burgen.txt")), 4);
        println!("{}", r.join("\n"));
    }

    #[test]
    fn grundrisse_sind_dicht_auf_zufallskarte() {
        let r = check_map(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/4-player-zufall-256-682070261.txt")), 4);
        println!("{}", r.join("\n"));
    }
}
