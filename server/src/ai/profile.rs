//! Wie ein Computergegner spielt: Bauliste, Armee, Mauer.
//!
//! Alles, was eine Stärke ausmacht, steht hier als Daten. Eine neue Stufe oder
//! Strategie ist ein neues `Profile`, der Ablauf in `mod.rs` bleibt gleich.

use protocol::{AiLevel, BuildingKind, Good, UnitKind};

use BuildingKind::*;

/// Spielweise eines Computergegners.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Alle so viele Spielsekunden überlegt er, was er als Nächstes tut.
    pub think_seconds: f32,
    /// Was er der Reihe nach baut: (Gebäude, so viele will er davon haben).
    /// Ein Eintrag ist erledigt, sobald er so viele hat.
    pub build_order: Vec<(BuildingKind, u32)>,
    /// Ausbauten, wenn die Bauliste durch ist: (Gebäude, auf diese Stufe), alle dieser Art.
    pub upgrades: Vec<(BuildingKind, u32)>,
    /// Höchstbestände: Betriebe, die das herstellen, halten an, sobald so viel im Vorrat liegt
    /// (Einstellung „Anhalten bei“). So frisst das Sägewerk nicht alles Holz.
    pub stock_targets: Vec<(Good, u32)>,
    /// So viele Soldaten will er haben.
    pub army_size: u32,
    /// Diese Soldaten bildet er der Reihe nach aus.
    pub army_units: Vec<UnitKind>,
    /// Woraus er seinen Mauerring baut (Palisade, Mauer, hohe Mauer).
    pub wall_kind: BuildingKind,
    /// Abstand des Mauerrings vom Bergfried in Feldern.
    pub wall_gap: u32,
    /// So viele Mauerstücke setzt er höchstens auf einmal.
    pub walls_per_think: u32,
    /// Mindestens so viel Holz hält er für Gebäude zurück, bevor er Mauern baut.
    pub wall_wood_reserve: u32,
}

impl Profile {
    pub fn for_level(level: AiLevel) -> Profile {
        match level {
            AiLevel::Easy => Profile {
                think_seconds: 8.0,
                build_order: vec![
                    (Woodcutter, 1),
                    (House, 1),
                    (Woodcutter, 2),
                    (Storehouse, 1),
                    (Orchard, 1),
                    (Granary, 1),
                    (Sawmill, 1),
                    (House, 2),
                    (CartShed, 1),
                    (Quarry, 1),
                    (SpearMaker, 1),
                    (Armory, 1),
                    (Woodcutter, 4),
                    (Barracks, 1),
                    (Woodcutter, 5),
                ],
                upgrades: vec![(CartShed, 2)],
                stock_targets: vec![(Good::Boards, 40), (Good::Stone, 80), (Good::Spear, 4)],
                army_size: 6,
                army_units: vec![UnitKind::Spearman],
                wall_kind: Palisade,
                wall_gap: 11,
                walls_per_think: 4,
                wall_wood_reserve: 30,
            },
            AiLevel::Normal => Profile {
                think_seconds: 4.0,
                build_order: vec![
                    (Woodcutter, 2),
                    (House, 1),
                    (Storehouse, 1),
                    (Orchard, 1),
                    (Granary, 1),
                    (Woodcutter, 4),
                    (Sawmill, 1),
                    (House, 2),
                    (CartShed, 1),
                    (Quarry, 1),
                    (Stonemason, 1),
                    (SpearMaker, 1),
                    (Armory, 1),
                    (Barracks, 1),
                    (Bowyer, 1),
                    (House, 3),
                    (Woodcutter, 7),
                    (Quarry, 2),
                    (Orchard, 2),
                ],
                upgrades: vec![(CartShed, 2), (House, 2), (Barracks, 2)],
                stock_targets: vec![
                    (Good::Boards, 60),
                    (Good::Stone, 100),
                    (Good::Blocks, 60),
                    (Good::Spear, 6),
                    (Good::Longbow, 6),
                ],
                army_size: 16,
                army_units: vec![UnitKind::Spearman, UnitKind::Archer],
                wall_kind: Wall,
                wall_gap: 12,
                walls_per_think: 8,
                wall_wood_reserve: 40,
            },
        }
    }
}
