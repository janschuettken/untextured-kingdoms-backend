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
    play_watch(map_text, levels, minutes, |_, _| {})
}

/// Wie `play_on`, ruft aber alle zehn Spielsekunden `watch` auf.
fn play_watch(map_text: &str, levels: &[AiLevel], minutes: u32, watch: impl FnMut(&World, &[Computer])) -> World {
    let seats: Vec<(AiLevel, &str)> = levels.iter().map(|&l| (l, "")).collect();
    play_seats(map_text, &seats, minutes, None, watch)
}

/// Computer mit Stärke und Charakter (Id aus `all`, ohne `all`: wie die Stufe).
fn play_seats(
    map_text: &str,
    seats: &[(AiLevel, &str)],
    minutes: u32,
    all: Option<&protocol::AiPersonalities>,
    mut watch: impl FnMut(&World, &[Computer]),
) -> World {
    let map = GameMap::parse(map_text).unwrap();
    let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
    let spawns = map.spawns.clone();
    let mut world = World::new(map, economy);
    let mut ais: Vec<Computer> = Vec::new();
    for (i, &(level, personality)) in seats.iter().enumerate() {
        let player = i as u32 + 1;
        world.spawn_player_at(player, spawns[i]);
        ais.push(match all {
            Some(all) => Computer::new(super::Seat { player, level, personality: personality.into(), daring: None, helpfulness: None }, all, &world.economy),
            None => Computer::with_level(player, level, &world.economy),
        });
    }
    let (ticks, _) = tokio::sync::broadcast::channel(64);
    // Gröbere Schritte als im Spiel (20 je Sekunde), damit der Test schnell bleibt.
    let dt = 0.25;
    let mut rejected = 0;
    for step in 0..(minutes as f32 * 60.0 / dt) as u32 {
        for ai in &mut ais {
            for cmd in ai.think(&world, dt) {
                if let Err(e) = apply_command(&mut world, &ticks, ai.player, cmd) {
                    println!("Spieler {}: abgelehnt {}", ai.player, e.key);
                    rejected += 1;
                }
            }
        }
        world.step(dt);
        if step % (10.0 / dt) as u32 == 0 {
            watch(&world, &ais);
        }
        for e in world.take_events() {
            if let protocol::ServerMsg::PlayerDefeated { player, by } = e {
                println!("min {:.0}: Spieler {player} ausgeschieden (König gefallen durch {by:?})", step as f32 * dt / 60.0);
            }
        }
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
    // Seit neue Betriebe im Umkreis des Wagenschuppens nicht mehr selbst tragen,
    // kommt der Computer in 40 Minuten auf etwas weniger Soldaten (vorher 6).
    assert!(stock.army.soldiers >= 4, "{:?}", stock.army);
    assert!(count(&world, BuildingKind::Gate) >= 1, "{:?} {:?}", stock.pools, stock.goods);
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
    // Schwer sperrt auf dieser Karte nur die Engstelle (Graben ringsum): wenige Mauern, aber dicht.
    assert!(count(&world, BuildingKind::HighWall) >= 5, "{kinds:?}");
    assert!(world.castle_status(1).closed, "Burg offen: {:?} {kinds:?}", world.castle_status(1));
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
        // Mit Kampf kann einer ausscheiden (König gefallen): dann wurde er erobert.
        assert!(n >= 12 || world.is_eliminated(p), "Spieler {p}: nur {n} Gebäude");
    }
    // Mindestens die beiden schweren halten durch.
    assert!(!world.is_eliminated(3) || !world.is_eliminated(4));
}

/// Der schwere Computer schätzt den leichten als schwächer ein und schickt eine Welle hin.
#[test]
fn schwerer_computer_greift_den_schwaecheren_an() {
    let mut launched = None;
    let mut farthest = 0.0f32;
    let world = play_watch(MAP, &[AiLevel::Hard, AiLevel::Easy], 70, |world, ais| {
        if let Some(c) = &ais[0].campaign {
            if launched.is_none() {
                println!("Welle nach {:.0} min: {} Soldaten gegen Spieler {}", ais[0].clock / 60.0, c.units.len(), c.target);
            }
            launched.get_or_insert((c.target, c.units.len()));
        }
        let keep = world.buildings.iter().find(|b| b.owner == 1 && b.kind == BuildingKind::Keep).unwrap();
        for u in world.units.iter().filter(|u| u.owner == 1 && u.kind.is_soldier()) {
            farthest = farthest.max((u.pos.0 - keep.x as f32).hypot(u.pos.1 - keep.y as f32));
        }
    });
    let (target, size) = launched.expect("keine Welle losgeschickt");
    assert_eq!(target, 2);
    assert!(size >= 10, "nur {size} Soldaten in der Welle");
    // Die Burgen liegen weit auseinander (keine Höchstentfernung).
    assert!(farthest > 100.0, "Welle nur {farthest:.0} Felder weit gekommen");
    let _ = world;
}

/// Charakter ohne Angriffslust schickt nie jemanden los.
#[test]
fn friedlicher_charakter_greift_nicht_an() {
    let mut p = super::personality::neutral();
    p.aggression = 0;
    let eco = crate::economy::Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
    let all = protocol::AiPersonalities { personalities: vec![p] };
    let c = Computer::new(super::Seat { player: 1, level: AiLevel::Hard, personality: String::new(), daring: None, helpfulness: None }, &all, &eco);
    assert!(c.profile.attack.is_none());
}

/// Die eingebauten Charaktere spielen alle (auf der Zufallskarte, schwer).
#[test]
fn alle_charaktere_bauen_eine_burg() {
    const RANDOM: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/4-player-zufall-256-682070261.txt"));
    let all = super::personality::parse(super::personality::PERSONALITIES_JSON).unwrap();
    let ids: Vec<&str> = all.personalities.iter().map(|p| p.id.as_str()).take(4).collect();
    let seats: Vec<(AiLevel, &str)> = ids.iter().map(|&id| (AiLevel::Normal, id)).collect();
    let world = play_seats(RANDOM, &seats, 45, Some(&all), |_, _| {});
    for (i, id) in ids.iter().enumerate() {
        let p = i as u32 + 1;
        let n = world.buildings.iter().filter(|b| b.owner == p && world.economy.building(b.kind).is_some_and(|d| d.road_speed_percent == 0)).count();
        let mut kinds = std::collections::BTreeMap::new();
        for b in world.buildings.iter().filter(|b| b.owner == p) {
            *kinds.entry(format!("{:?}", b.kind)).or_insert(0) += 1;
        }
        let st = world.castle_status(p);
        println!("{id}: {n} Gebäude, {} Soldaten, Burg zu {} {kinds:?}", world.stock_state(p).army.soldiers, st.closed);
        assert!(n >= 12, "{id}: nur {n} Gebäude");
    }
}

/// Verbündeter (2) wird von Spieler 3 bedrängt: Spieler 1 schickt einen Teil seiner Soldaten
/// und Waren, aber nicht alle; ohne Hilfsbereitschaft niemanden.
#[test]
fn hilft_dem_verbuendeten_aber_nicht_mit_allen() {
    use protocol::{ClientMsg, UnitKind};
    const RANDOM: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/maps/4-player-zufall-256-682070261.txt"));
    let setup = |help: u8| {
        let map = GameMap::parse(RANDOM).unwrap();
        let spawns = map.spawns.clone();
        let mut world = World::new(map, Economy::parse(crate::economy::ECONOMY_JSON).unwrap());
        for p in 1..=3 {
            world.spawn_player_at(p, spawns[p as usize - 1]);
        }
        world.set_team(1, 1);
        world.set_team(2, 1);
        world.give_everything_for_tests(1);
        let home = super::attack::keep_center(&world, 1).unwrap();
        for i in 0..14 {
            let id = 50_000 + i;
            world.units.push(crate::world::Unit::new(id, 1, UnitKind::Spearman, (home.0 + 6.0 + (i % 4) as f32, home.1 + 6.0 + (i / 4) as f32)));
        }
        let ally = super::attack::keep_center(&world, 2).unwrap();
        for i in 0..3 {
            world.units.push(crate::world::Unit::new(60_000 + i, 3, UnitKind::Swordsman, (ally.0 + 8.0 + i as f32, ally.1 + 8.0)));
        }
        let all = protocol::AiPersonalities { personalities: vec![super::personality::neutral()] };
        let seat = super::Seat { player: 1, level: AiLevel::Normal, personality: String::new(), daring: None, helpfulness: Some(help) };
        let mut ai = Computer::new(seat, &all, &world.economy);
        let mut cmds = Vec::new();
        for _ in 0..20 {
            cmds.extend(ai.think(&world, 0.25));
        }
        // Nur Befehle, die zum Verbündeten führen.
        let relief: Vec<usize> = cmds
            .iter()
            .filter_map(|c| match c {
                ClientMsg::MoveUnits { unit_ids, x, y } if (x - ally.0).hypot(y - ally.1) < 30.0 => Some(unit_ids.len()),
                _ => None,
            })
            .collect();
        (cmds, relief)
    };
    let (cmds, sent) = setup(50);
    println!("geschickt: {sent:?}");
    assert!(!sent.is_empty(), "keine Hilfe");
    // 14 Speerträger, 4 bleiben daheim, höchstens die Hälfte der übrigen, und nur so viele
    // wie gegen drei Schwertkämpfer nötig.
    assert!(sent[0] >= 2 && sent[0] <= 5, "{} geschickt", sent[0]);
    assert!(cmds.iter().any(|c| matches!(c, ClientMsg::SendGoods { to: 2, goods, .. } if !goods.is_empty())), "Waren für den Verbündeten");
    let (none, sent) = setup(0);
    assert!(sent.is_empty());
    assert!(!none.iter().any(|c| matches!(c, ClientMsg::SendGoods { .. })));
}
