//! Gebäudearten und ihre festen Werte.
//!
//! Erster Balancing-Vorschlag, bewusst einfach gehalten. Kosten kommen mit
//! den Ressourcen dazu.

use protocol::{BuildingKind, BuildingSpec};

pub const ALL_KINDS: [BuildingKind; 5] = [
    BuildingKind::Wall,
    BuildingKind::HighWall,
    BuildingKind::Tower,
    BuildingKind::Keep,
    BuildingKind::Storehouse,
];

pub fn spec(kind: BuildingKind) -> BuildingSpec {
    let (w, h, max_hp) = match kind {
        BuildingKind::Wall => (1, 1, 300),
        BuildingKind::HighWall => (1, 1, 700),
        BuildingKind::Tower => (2, 2, 900),
        BuildingKind::Keep => (4, 4, 3000),
        BuildingKind::Storehouse => (3, 3, 600),
    };
    BuildingSpec { kind, w, h, max_hp }
}

/// Höchstens so viele Gebäude dieser Art pro Spieler (None = beliebig viele).
pub fn limit_per_player(kind: BuildingKind) -> Option<usize> {
    match kind {
        BuildingKind::Keep => Some(1),
        _ => None,
    }
}

pub fn all_specs() -> Vec<BuildingSpec> {
    ALL_KINDS.iter().map(|&k| spec(k)).collect()
}
