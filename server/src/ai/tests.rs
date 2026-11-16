//! Ein Computergegner spielt allein auf der Standardkarte.

use protocol::{AiLevel, BuildingKind};

use super::Computer;
use crate::{economy::Economy, game::apply_command, map::GameMap, world::World};

const MAP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/zwei-burgen.txt"));

/// `minutes` Spielminuten mit einem Computergegner (Spieler 1) simulieren.
fn play(level: AiLevel, minutes: u32) -> World {
    play_many(&[level], minutes)
}

/// Mehrere Computergegner (Spieler 1, 2, ...) auf den ersten Startplätzen.
fn play_many(levels: &[AiLevel], minutes: u32) -> World {
    play_on(MAP, levels, minutes)
}

fn play_on(map_text: &str, levels: &[AiLevel], minutes: u32) -> World {
    let map = GameMap::parse(map_text).unwrap();
    let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
    let spawns = map.spawns.clone();
    let mut world = World::new(map, economy);
    let mut ais: Vec<Computer> = Vec::new();
    for (i, &level) in levels.iter().enumerate() {
        world.spawn_player_at(i as u32 + 1, spawns[i]);
        ais.push(Computer::new(i as u32 + 1, level));
    }
    let (ticks, _) = tokio::sync::broadcast::channel(64);
    // Gröbere Schritte als im Spiel (20 je Sekunde), damit der Test schnell bleibt.
    let dt = 0.25;
    let mut rejected = 0;
    for _ in 0..(minutes as f32 * 60.0 / dt) as u32 {
        for ai in &mut ais {
            for cmd in ai.think(&world, dt) {
                if let Err(e) = apply_command(&mut world, &ticks, ai.player, cmd) {
                    println!("Spieler {}: abgelehnt {}", ai.player, e.key);
                    rejected += 1;
                }
            }
        }
        world.step(dt);
        let _ = world.take_events();
        let _ = world.take_dirty_stocks();
    }
    assert!(rejected < 5, "{rejected} Befehle abgelehnt: er soll vorher prüfen");
    world
}

fn count(world: &World, kind: BuildingKind) -> usize {
    world.buildings.iter().filter(|b| b.owner == 1 && b.kind == kind).count()
}

#[test]
fn leichter_computer_baut_wirtschaft_armee_und_mauer() {
    let world = play(AiLevel::Easy, 40);
    let summary: Vec<_> = [
        BuildingKind::Woodcutter,
        BuildingKind::House,
        BuildingKind::Storehouse,
        BuildingKind::Orchard,
        BuildingKind::Sawmill,
        BuildingKind::Barracks,
        BuildingKind::Gate,
        BuildingKind::Palisade,
    ]
    .iter()
    .map(|&k| (k, count(&world, k)))
    .collect();
    let stock = world.stock_state(1);
    assert!(count(&world, BuildingKind::Woodcutter) >= 2, "{summary:?}");
    assert!(count(&world, BuildingKind::House) >= 2, "{summary:?}");
    assert!(count(&world, BuildingKind::Barracks) >= 1, "{summary:?}");
    assert!(stock.army.soldiers >= 1, "{summary:?} {:?}", stock.army);
    assert!(count(&world, BuildingKind::Gate) >= 1, "{summary:?}");
    assert!(count(&world, BuildingKind::Palisade) >= 10, "{summary:?}");
}

#[test]
fn normaler_computer_baut_mehr() {
    let world = play(AiLevel::Normal, 40);
    let stock = world.stock_state(1);
    assert!(count(&world, BuildingKind::Woodcutter) >= 4);
    assert!(count(&world, BuildingKind::Barracks) >= 1);
    assert!(stock.army.soldiers >= 6, "{:?}", stock.army);
    assert!(count(&world, BuildingKind::Gate) >= 1);
}

#[test]
fn schwerer_computer_baut_torhaus_wege_und_grosse_armee() {
    let world = play(AiLevel::Hard, 60);
    let stock = world.stock_state(1);
    let roads = count(&world, BuildingKind::StonePath);
    let mut kinds = std::collections::BTreeMap::new();
    for b in world.buildings.iter().filter(|b| b.owner == 1) {
        *kinds.entry(format!("{:?}", b.kind)).or_insert(0) += 1;
    }
    assert!(roads >= 5, "nur {roads} Wegfelder: {kinds:?}");
    assert!(count(&world, BuildingKind::HighWall) >= 10);
    assert!(count(&world, BuildingKind::Market) >= 1);
    assert!(count(&world, BuildingKind::Barracks) >= 2);
    assert!(stock.army.soldiers >= 15, "{:?}", stock.army);
    let on_keep = world.units.iter().filter(|u| u.owner == 1 && u.is_on_wall()).count();
    assert!(on_keep >= 1, "keine Bogenschützen oben");
}

#[test]
fn zwei_computer_kommen_sich_nicht_in_die_quere() {
    let world = play_many(&[AiLevel::Easy, AiLevel::Normal], 30);
    for p in [1, 2] {
        let n = world.buildings.iter().filter(|b| b.owner == p).count();
        assert!(n >= 15, "Spieler {p}: nur {n} Gebäude");
    }
}

/// Zufallskarte mit Flüssen und Furten, vier Computer aller Stärken.
#[test]
fn vier_computer_auf_zufallskarte_mit_fluessen() {
    const RANDOM: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/4-player-zufall-256-682070261.txt"));
    let world = play_on(RANDOM, &[AiLevel::Easy, AiLevel::Normal, AiLevel::Hard, AiLevel::Hard], 45);
    for p in 1..=4 {
        let n = world.buildings.iter().filter(|b| b.owner == p && world.economy.building(b.kind).is_some_and(|d| d.road_speed_percent == 0)).count();
        let kinds = |k: BuildingKind| world.buildings.iter().filter(|b| b.owner == p && b.kind == k).count();
        println!(
            "Spieler {p}: {n} Gebäude, {} Soldaten, Wege {}, Tore {}+{}, Mauer {}, Türme {}, oben {}",
            world.stock_state(p).army.soldiers,
            kinds(BuildingKind::WoodPath) + kinds(BuildingKind::StonePath),
            kinds(BuildingKind::Gate),
            kinds(BuildingKind::Gatehouse),
            kinds(BuildingKind::Palisade) + kinds(BuildingKind::Wall) + kinds(BuildingKind::HighWall),
            kinds(BuildingKind::Tower),
            world.units.iter().filter(|u| u.owner == p && u.is_on_wall()).count()
        );
        assert!(n >= 12, "Spieler {p}: nur {n} Gebäude");
    }
}
