//! Angriffe: Stärke der Gegner grob einschätzen, eine Welle sammeln, hinmarschieren.
//!
//! Kampf gibt es noch nicht. Die Welle läuft trotzdem schon los, bleibt eine Weile vor
//! der gegnerischen Burg (so weit sie kommt, vor geschlossenen Toren und Mauern) und
//! geht dann wieder heim. Sobald Einheiten kämpfen, kämpfen sie dort; Verluste lassen
//! die Welle umkehren (`retreat_below`).
//!
//! Geschätzt wird aus dem, was jeder Spieler sieht: Soldaten auf der Karte (oben auf
//! Mauern zählen mehr), Türme und eine geschlossene Mauer. Eine Höchstentfernung gibt
//! es nicht: das Ziel ist der schwächste Gegner, bei Gleichstand der nächste.

use protocol::{BuildingKind, UnitKind};

use crate::world::World;

/// Wert eines Soldaten: Leben mal Schaden, Fernkämpfer etwas mehr.
pub fn unit_power(world: &World, kind: UnitKind) -> f32 {
    world.economy.unit(kind).map_or(0.0, |d| {
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
        .filter(|b| b.owner != me && b.owner != 0 && b.kind == BuildingKind::Keep)
        .map(|b| {
            let c = b.entrance.unwrap_or((b.x + b.w / 2, b.y + b.h / 2));
            let d = (c.0 as f32 - from.0).hypot(c.1 as f32 - from.1);
            (b.owner, estimate(world, b.owner), d, c)
        })
        .collect();
    targets.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)));
    let start = (from.0 as i32, from.1 as i32);
    for (owner, power, _, c) in targets {
        let path = world.map.nav.find_path(start, (c.0 as i32, c.1 as i32));
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
