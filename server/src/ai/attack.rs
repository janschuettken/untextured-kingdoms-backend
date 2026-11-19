//! Angriffe: Stärke der Gegner grob einschätzen, eine Welle sammeln, hinmarschieren.
//!
//! Die Welle läuft zur gegnerischen Burg (so weit sie kommt, vor geschlossenen Toren und
//! Mauern), kämpft dort mit allem in Reichweite (Kampf-Grundgerüst in `world/combat.rs`),
//! bleibt eine Weile und geht dann wieder heim. Verluste lassen sie früher umkehren
//! (`retreat_below`). Mauern und Tore angreifen kann sie noch nicht.
//!
//! Geschätzt wird aus dem, was jeder Spieler sieht: Soldaten auf der Karte (oben auf
//! Mauern zählen mehr), Türme und eine geschlossene Mauer. Eine Höchstentfernung gibt
//! es nicht: das Ziel ist der schwächste Gegner, bei Gleichstand der nächste. Verbündete
//! greift er nie an.

use protocol::{BuildingKind, UnitKind};

use crate::world::World;

/// Wert eines Soldaten: Leben mal Schaden, Fernkämpfer etwas mehr; Heiler nach ihrer Heilung.
pub fn unit_power(world: &World, kind: UnitKind) -> f32 {
    world.economy.unit(kind).map_or(0.0, |d| {
        if d.attack == 0 && d.heal > 0 {
            return d.hp as f32 * d.heal as f32 / 100.0;
        }
        let ranged = if d.range > 2.0 { 1.3 } else { 1.0 };
        d.hp as f32 * d.attack as f32 * ranged / 100.0
    })
}

/// Alle Soldaten des Spielers zusammen; wer oben auf der Mauer steht, zählt anderthalbfach
/// (höher stehen hilft im Kampf).
pub fn army_power(world: &World, player: u32) -> f32 {
    world
        .units
        .iter()
        .filter(|u| u.owner == player && u.kind.is_soldier())
        .map(|u| unit_power(world, u.kind) * if u.is_on_wall() { 1.5 } else { 1.0 })
        .sum()
}

/// Wie viel die Befestigung hilft: Türme, und ob man erst durch eine Mauer muss.
pub fn defense_factor(world: &World, player: u32) -> f32 {
    let towers = world
        .buildings
        .iter()
        .filter(|b| b.owner == player && matches!(b.kind, BuildingKind::Tower | BuildingKind::HighTower))
        .count() as f32;
    let closed = if crate::world::castle_closed(world, player) { 0.5 } else { 0.0 };
    1.0 + closed + (towers * 0.1).min(0.6)
}

/// Grobe Stärke eines Spielers, so wie ein Angreifer sie sieht.
pub fn estimate(world: &World, player: u32) -> f32 {
    army_power(world, player) * defense_factor(world, player)
}

/// Eine Welle unterwegs. Nicht gespeichert: nach dem Laden holt der Sammelpunkt sie heim.
#[derive(Debug, Clone)]
pub struct Campaign {
    pub target: u32,
    pub units: Vec<u32>,
    /// Stärke beim Aufbruch.
    pub start_power: f32,
    /// Wohin (nächstes erreichbares Feld an der gegnerischen Burg).
    pub goal: (f32, f32),
    /// Seit wann alle angekommen sind bzw. nicht weiterkommen.
    pub arrived: Option<f32>,
}

/// Ziel einer Welle: der Spieler mit der kleinsten geschätzten Stärke, dazu die Stelle,
/// bis zu der man von `from` aus kommt. Ohne Weg dorthin (Insel) der nächste.
pub fn choose_target(world: &World, me: u32, from: (f32, f32)) -> Option<(u32, f32, (f32, f32))> {
    let mut targets: Vec<(u32, f32, f32, (u32, u32))> = world
        .buildings
        .iter()
        .filter(|b| b.owner != me && b.owner != 0 && b.kind == BuildingKind::Keep && world.can_attack(me, b.owner))
        .map(|b| {
            let c = b.entrance.unwrap_or((b.x + b.w / 2, b.y + b.h / 2));
            let d = (c.0 as f32 - from.0).hypot(c.1 as f32 - from.1);
            (b.owner, estimate(world, b.owner), d, c)
        })
        .collect();
    targets.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)));
    let start = (from.0 as i32, from.1 as i32);
    // Feindliche Tore gelten beim Bewegen als zu: die Welle läuft bis vor das erste Tor.
    let gates: std::collections::HashSet<(i32, i32)> = world
        .buildings
        .iter()
        .filter(|b| crate::world::is_gate(b.kind) && !world.allied(me, b.owner))
        .flat_map(crate::world::passage_tiles)
        .map(|(x, y)| (x as i32, y as i32))
        .collect();
    for (owner, power, _, c) in targets {
        let mut path = world.map.nav.find_path(start, (c.0 as i32, c.1 as i32));
        if let Some(at) = path.iter().position(|p| gates.contains(p)) {
            path.truncate(at.saturating_sub(2));
        }
        let Some(&end) = path.last() else { continue };
        // Nicht wenigstens halbwegs hingekommen (etwa Wasser dazwischen)? Nächster.
        let left = ((end.0 - c.0 as i32) as f32).hypot((end.1 - c.1 as i32) as f32);
        let total = ((start.0 - c.0 as i32) as f32).hypot((start.1 - c.1 as i32) as f32);
        if left > total * 0.5 && left > 30.0 {
            continue;
        }
        return Some((owner, power, (end.0 as f32 + 0.5, end.1 as f32 + 0.5)));
    }
    None
}

/// Hilfe für einen angegriffenen Verbündeten. Nicht gespeichert, wie die Welle.
#[derive(Debug, Clone)]
pub struct Relief {
    pub ally: u32,
    pub units: Vec<u32>,
    pub start_power: f32,
    /// Spätestens dann geht sie wieder heim.
    pub until: f32,
}

/// Mittelpunkt des Bergfrieds eines Spielers.
pub fn keep_center(world: &World, player: u32) -> Option<(f32, f32)> {
    world
        .buildings
        .iter()
        .find(|b| b.owner == player && b.kind == BuildingKind::Keep)
        .map(|b| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0))
}

/// Feindliche Soldaten (Feinde von `me`, die `defender` angreifen dürfen) nahe am Bergfried
/// von `defender`, der nächste zuerst.
pub fn threats(world: &World, defender: u32, me: u32, radius: f32) -> Vec<&crate::world::Unit> {
    let Some(c) = keep_center(world, defender) else { return Vec::new() };
    let dist = |u: &crate::world::Unit| (u.pos.0 - c.0).hypot(u.pos.1 - c.1);
    let mut list: Vec<&crate::world::Unit> = world
        .units
        .iter()
        .filter(|u| u.kind.is_soldier() && world.can_attack(me, u.owner) && world.can_attack(defender, u.owner) && dist(u) < radius)
        .collect();
    list.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    list
}

/// Wie stark eine Welle mindestens sein muss: `superiority` mal der Gegner, mit Mut
/// (`daring`, 0 bis 1) weniger, und umso weniger, je länger er schon wartet (in Pausen
/// zwischen zwei Wellen gemessen). Mut 0: nie schwächer; Mut 1: sofort, egal wie stark.
pub fn needed_ratio(superiority: f32, daring: f32, waited_cooldowns: f32) -> f32 {
    superiority * (1.0 - daring * (1.0 + waited_cooldowns.max(0.0))).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::needed_ratio;

    #[test]
    fn mut_senkt_die_huerde_mit_der_zeit() {
        assert_eq!(needed_ratio(1.3, 0.0, 100.0), 1.3, "ohne Mut nie schwächer");
        assert_eq!(needed_ratio(1.3, 1.0, 0.0), 0.0, "ständig");
        let a = needed_ratio(1.3, 0.3, 0.0);
        let b = needed_ratio(1.3, 0.3, 1.0);
        assert!(a < 1.3 && b < a);
        assert_eq!(needed_ratio(1.3, 0.3, 3.0), 0.0, "nach langem Warten greift er an");
    }
}
