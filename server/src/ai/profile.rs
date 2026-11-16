//! Wie ein Computergegner spielt: Bauliste, Armee, Mauer, Wege, Handel.
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
    /// So viele Soldaten stehen höchstens je Kaserne in der Warteschlange.
    pub queue_per_barracks: u32,
    /// Fernkämpfer (Reichweite über 2) schickt er auf Bergfried und Türme statt zum Sammelpunkt.
    pub garrison_ranged: bool,
    /// Woraus er seinen Mauerring baut (Palisade, Mauer, hohe Mauer).
    pub wall_kind: BuildingKind,
    /// Tor auf der Seite, zu der der Bergfried schaut (Tor oder großes Torhaus); die
    /// anderen Seiten bekommen ein einfaches Tor.
    pub main_gate: BuildingKind,
    /// Türme an den vier Ecken des Rings, wenn die Mauer steht.
    pub corner_towers: bool,
    /// Abstand des Mauerrings vom Bergfried in Feldern.
    pub wall_gap: u32,
    /// So viele Mauerstücke setzt er höchstens auf einmal.
    pub walls_per_think: u32,
    /// Mindestens so viel Holz hält er für Gebäude zurück, bevor er Mauern und Wege baut.
    pub wall_wood_reserve: u32,
    /// Weg (Holz- oder Pflasterweg), `None` = baut keine.
    pub road_kind: Option<BuildingKind>,
    /// Straßen zu allen vier Toren (sonst nur zum Haupttor).
    pub roads_all_sides: bool,
    /// Wege vom Tor hinaus zu den Abbaubetrieben (so weit der Bauradius reicht).
    pub roads_to_producers: bool,
    /// Wege vor der Mauer legen (sonst nur mit dem, was die Mauer übrig lässt).
    pub roads_first: bool,
    /// So viele Wegfelder legt er höchstens auf einmal.
    pub roads_per_think: u32,
    /// Handel am Markt (braucht einen Markt in der Bauliste).
    pub trade: Option<Trade>,
}

/// Was er am Markt tut.
#[derive(Debug, Clone)]
pub struct Trade {
    /// Über dieser Menge verkauft er den Rest.
    pub sell_above: Vec<(Good, u32)>,
    /// Bis zu dieser Menge kauft er nach, wenn das Geld reicht, aber erst wenn er das
    /// Gebäude hat, das die Ware braucht.
    pub buy_up_to: Vec<(Good, u32, BuildingKind)>,
    /// Kauft zu, was dem nächsten Gebäude der Bauliste fehlt.
    pub buy_missing: bool,
    /// So viele Taler behält er immer (Sold, Ausbildung).
    pub taler_reserve: u32,
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
                queue_per_barracks: 1,
                garrison_ranged: false,
                wall_kind: Palisade,
                main_gate: Gate,
                corner_towers: false,
                wall_gap: 11,
                walls_per_think: 4,
                wall_wood_reserve: 15,
                road_kind: Some(WoodPath),
                roads_all_sides: false,
                roads_to_producers: false,
                roads_first: false,
                roads_per_think: 3,
                trade: None,
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
                queue_per_barracks: 2,
                garrison_ranged: true,
                wall_kind: Wall,
                main_gate: Gate,
                corner_towers: false,
                wall_gap: 12,
                walls_per_think: 8,
                wall_wood_reserve: 25,
                road_kind: Some(WoodPath),
                roads_all_sides: true,
                roads_to_producers: true,
                roads_first: false,
                roads_per_think: 6,
                trade: None,
            },
            AiLevel::Hard => Profile {
                think_seconds: 2.0,
                build_order: vec![
                    (Woodcutter, 3),
                    (House, 1),
                    (Storehouse, 1),
                    (Orchard, 1),
                    (Granary, 1),
                    (Woodcutter, 5),
                    (Sawmill, 1),
                    (House, 2),
                    (CartShed, 1),
                    (Quarry, 1),
                    (SpearMaker, 1),
                    (Armory, 1),
                    (Barracks, 1),
                    (Woodcutter, 8),
                    (Bowyer, 1),
                    (House, 3),
                    (Stonemason, 1),
                    (Market, 1),
                    (Quarry, 2),
                    (Sawmill, 2),
                    (Smithy, 1),
                    (Barracks, 2),
                    (House, 4),
                    (Stonemason, 2),
                ],
                upgrades: vec![(CartShed, 3), (House, 2), (Barracks, 2), (HighWall, 2)],
                stock_targets: vec![
                    (Good::Boards, 80),
                    (Good::Stone, 120),
                    (Good::Blocks, 100),
                    (Good::Spear, 8),
                    (Good::Longbow, 8),
                    (Good::Sword, 6),
                ],
                army_size: 24,
                army_units: vec![UnitKind::Spearman, UnitKind::Archer, UnitKind::LightSwordsman, UnitKind::Archer],
                queue_per_barracks: 3,
                garrison_ranged: true,
                wall_kind: HighWall,
                main_gate: Gatehouse,
                corner_towers: true,
                wall_gap: 12,
                walls_per_think: 12,
                wall_wood_reserve: 30,
                road_kind: Some(StonePath),
                roads_all_sides: true,
                roads_to_producers: true,
                roads_first: true,
                roads_per_think: 10,
                trade: Some(Trade {
                    sell_above: vec![(Good::Fruit, 150), (Good::Stone, 200), (Good::Meat, 150)],
                    // Eisen für Schwerter und das Torhaus, leichte Rüstung für Schwertkämpfer.
                    buy_up_to: vec![(Good::Iron, 6, Smithy), (Good::LightArmor, 2, Smithy)],
                    buy_missing: true,
                    taler_reserve: 60,
                }),
            },
        }
    }
}
