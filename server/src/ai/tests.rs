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
            world.self_carry.insert(ai.player);
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
        BuildingKind::Wall,
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
    // Mauer aus Stein, wo der reicht, sonst erst Palisade.
    assert!(count(&world, BuildingKind::Palisade) + count(&world, BuildingKind::Wall) >= 10, "{summary:?}");
}

#[test]
fn normaler_computer_baut_mehr() {
    let world = play(AiLevel::Normal, 40);
    let stock = world.stock_state(1);
    assert!(count(&world, BuildingKind::Woodcutter) >= 4);
    assert!(count(&world, BuildingKind::Barracks) >= 1);
    assert!(stock.army.soldiers >= 6, "{:?}", stock.army);
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
    // Schwer sperrt auf dieser Karte nur die Engstelle (Graben ringsum): wenige Mauern, aber dicht,
    // mit Zinnen (Stufe 2).
    let walls: Vec<_> = world.buildings.iter().filter(|b| b.owner == 1 && b.kind == BuildingKind::Wall).collect();
    assert!(walls.len() >= 5, "{kinds:?}");
    assert!(walls.iter().filter(|b| b.level >= 2).count() >= 3, "keine Zinnen: {kinds:?}");
    assert!(world.castle_status(1).closed, "Burg offen: {:?} {kinds:?}", world.castle_status(1));
    assert!(count(&world, BuildingKind::Market) >= 1);
    assert!(count(&world, BuildingKind::Barracks) >= 2);
    assert!(stock.army.soldiers >= 15, "{:?}", stock.army);
    let on_keep = world.units.iter().filter(|u| u.owner == 1 && u.is_on_wall()).count();
    assert!(on_keep >= 1, "keine Bogenschützen oben");
    assert!(count(&world, BuildingKind::Apothecary) >= 1, "{kinds:?}");
    let healers = world.units.iter().filter(|u| u.owner == 1 && u.kind == protocol::UnitKind::Healer).count();
    assert!(healers >= 1, "keine Heiler");
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
        let n = world.buildings.iter().filter(|b| b.owner == p && world.economy.building(b.kind).is_some_and(|d| !d.is_road())).count();
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
        let n = world.buildings.iter().filter(|b| b.owner == p && world.economy.building(b.kind).is_some_and(|d| !d.is_road())).count();
        let mut kinds = std::collections::BTreeMap::new();
        for b in world.buildings.iter().filter(|b| b.owner == p) {
            *kinds.entry(format!("{:?}", b.kind)).or_insert(0) += 1;
        }
        let st = world.castle_status(p);
        println!("{id}: {n} Gebäude, {} Soldaten, Burg zu {} {kinds:?}", world.stock_state(p).army.soldiers, st.closed);
        assert!(n >= 12, "{id}: nur {n} Gebäude");
    }
}

/// Verlauf zum Ansehen (nicht im normalen Testlauf): `cargo test -p server verlauf -- --ignored --nocapture`.
/// Umgebungsvariablen: AI_MAP (Dateiname in maps/), AI_LEVELS (z. B. "hard,normal,easy,easy"), AI_MINUTES.
#[test]
#[ignore]
fn verlauf() {
    let map = std::env::var("AI_MAP").unwrap_or("4-player-zufall-256-682070261.txt".into());
    let text = std::fs::read_to_string(format!("{}/maps/{map}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let levels: Vec<AiLevel> = std::env::var("AI_LEVELS")
        .unwrap_or("hard,normal,easy".into())
        .split(',')
        .map(|s| match s {
            "easy" => AiLevel::Easy,
            "normal" => AiLevel::Normal,
            _ => AiLevel::Hard,
        })
        .collect();
    let minutes: u32 = std::env::var("AI_MINUTES").ok().and_then(|s| s.parse().ok()).unwrap_or(40);
    let mut tick = 0;
    let n = levels.len() as u32;
    let world = play_watch(&text, &levels, minutes, |world, ais| {
        tick += 1;
        if tick % 12 != 1 {
            return;
        }
        for p in 1..=n {
            if world.is_eliminated(p) {
                continue;
            }
            let kinds = |k: BuildingKind| world.buildings.iter().filter(|b| b.owner == p && b.kind == k).count();
            let walls: Vec<_> = world.buildings.iter().filter(|b| b.owner == p && matches!(b.kind, BuildingKind::Palisade | BuildingKind::Wall | BuildingKind::HighWall)).collect();
            let st = world.stock_state(p);
            let g = |good: protocol::Good| st.goods.iter().find(|x| x.good == good).map_or(0, |x| x.amount);
            let camp = ais[(p - 1) as usize].campaign.as_ref().map_or(String::new(), |c| format!(" WELLE {}→{}", c.units.len(), c.target));
            println!(
                "min {:>2} P{p}: geb {:>3} stbr {} pal {:>3} zu {} mauer {:>3} (L2 {:>3}) tore {}+{} türme {} sold {:>2} oben {:>2} armb {} | holz {} br {} st {} bl {} eisen {} taler {}{camp}",
                ais[0].clock as u32 / 60,
                world.buildings.iter().filter(|b| b.owner == p).count(),
                kinds(BuildingKind::Quarry),
                kinds(BuildingKind::Palisade),
                if world.castle_status(p).closed { "J" } else { "n" },
                walls.len(),
                walls.iter().filter(|b| b.level >= 2).count(),
                kinds(BuildingKind::Gate),
                kinds(BuildingKind::Gatehouse),
                kinds(BuildingKind::Tower) + kinds(BuildingKind::HighTower),
                st.army.soldiers,
                world.units.iter().filter(|u| u.owner == p && u.is_on_wall()).count(),
                world.units.iter().filter(|u| u.owner == p && u.kind == protocol::UnitKind::Crossbowman).count(),
                g(protocol::Good::Wood),
                g(protocol::Good::Boards),
                g(protocol::Good::Stone),
                g(protocol::Good::Blocks),
                g(protocol::Good::Iron),
                st.taler,
            );
        }
    });
    for p in 1..=n {
        println!("Spieler {p}: ausgeschieden {}", world.is_eliminated(p));
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
                ClientMsg::MoveUnits { unit_ids, x, y, .. } if (x - ally.0).hypot(y - ally.1) < 30.0 => Some(unit_ids.len()),
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

/// Wo die Rechenzeit hingeht (nicht im normalen Testlauf), im echten Takt (20 Ticks je Sekunde):
/// `cargo test --release -p server lastprofil -- --ignored --nocapture`.
/// Umgebungsvariablen: AI_MAP, AI_PLAYERS (Standard 4), AI_MINUTES (Standard 30).
#[test]
#[ignore]
fn lastprofil() {
    use crate::profile::{Phase, PhaseTimes, PHASE_COUNT, PHASE_NAMES};
    use std::time::Instant;
    let map = std::env::var("AI_MAP").unwrap_or("4-player-zufall-256-682070261.txt".into());
    let text = std::fs::read_to_string(format!("{}/maps/{map}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let players: usize = std::env::var("AI_PLAYERS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let minutes: u32 = std::env::var("AI_MINUTES").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let all = super::personality::parse(super::personality::PERSONALITIES_JSON).unwrap();
    let map = GameMap::parse(&text).unwrap();
    let economy = Economy::parse(crate::economy::ECONOMY_JSON).unwrap();
    let spawns = map.spawns.clone();
    let mut world = World::new(map, economy);
    let mut ais: Vec<Computer> = Vec::new();
    for (i, p) in all.personalities.iter().take(players).enumerate() {
        let player = i as u32 + 1;
        world.spawn_player_at(player, spawns[i]);
        let seat = super::Seat { player, level: AiLevel::Hard, personality: p.id.clone(), daring: None, helpfulness: None };
        ais.push(Computer::new(seat, &all, &world.economy));
    }
    let (ticks, _) = tokio::sync::broadcast::channel(64);
    let dt = 0.05;
    let mut rows: Vec<(u32, PhaseTimes)> = Vec::new();
    for step in 0..(minutes as f32 * 60.0 / dt) as u32 {
        let started = Instant::now();
        let mut phases = PhaseTimes::default();
        phases.time(Phase::Ai, || {
            let thinking = Instant::now();
            for ai in &mut ais {
                if ai.due(dt as f32) && thinking.elapsed() > std::time::Duration::from_millis(8) {
                    ai.postpone(dt as f32);
                    continue;
                }
                world.self_carry.insert(ai.player);
                for cmd in ai.think(&world, dt) {
                    let _ = apply_command(&mut world, &ticks, ai.player, cmd);
                }
            }
        });
        world.phase_us = PhaseTimes::default();
        world.step(dt);
        phases.merge(&world.phase_us);
        phases.time(Phase::Send, || {
            let _ = world.take_events();
            let _ = world.take_dirty_stocks();
            let _ = world.snapshot();
        });
        if step % (10 * 20) == 0 {
            phases.time(Phase::Castle, || {
                for p in 1..=players as u32 {
                    let _ = world.castle_status(p);
                }
            });
        }
        if step % (60 * 20) == 0 {
            phases.time(Phase::Save, || serde_json::to_string(&world.to_save("x")).unwrap().len());
        }
        rows.push((started.elapsed().as_micros() as u32, phases));
        if step % (60 * 20) == 0 {
            let last: Vec<u32> = rows.iter().rev().take(1200).map(|r| r.0).collect();
            let avg = last.iter().sum::<u32>() as f32 / last.len() as f32 / 1000.0;
            let max = *last.iter().max().unwrap() as f32 / 1000.0;
            println!("min {:>3}: {} Einheiten, {} Gebäude, Schnitt {avg:.2} ms, Spitze {max:.1} ms", step / 1200, world.units.len(), world.buildings.len());
        }
    }
    let n = rows.len() as f32;
    let mut sum = [0u64; PHASE_COUNT];
    let mut max = [0u32; PHASE_COUNT];
    for (_, p) in &rows {
        for i in 0..PHASE_COUNT {
            sum[i] += p.0[i] as u64;
            max[i] = max[i].max(p.0[i]);
        }
    }
    let mut order: Vec<usize> = (0..PHASE_COUNT).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sum[i]));
    println!("{:<12} {:>9} {:>9}", "Teil", "Schnitt", "Spitze");
    for i in order {
        println!("{:<12} {:>7.3}ms {:>7.1}ms", PHASE_NAMES[i], sum[i] as f32 / n / 1000.0, max[i] as f32 / 1000.0);
    }
    let late = rows.iter().filter(|r| r.0 > 50_000).count();
    let over10 = rows.iter().filter(|r| r.0 > 10_000).count();
    println!("Ticks: {}, über 10 ms: {over10}, über 50 ms: {late}", rows.len());
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    for (total, p) in rows.iter().take(15) {
        let mut parts: Vec<(usize, u32)> = p.0.iter().copied().enumerate().filter(|&(_, v)| v > 500).collect();
        parts.sort_by_key(|&(_, v)| std::cmp::Reverse(v));
        let text: Vec<String> = parts.iter().map(|&(i, v)| format!("{} {:.1}", PHASE_NAMES[i], v as f32 / 1000.0)).collect();
        println!("{:>6.1} ms: {}", *total as f32 / 1000.0, text.join(", "));
    }
}

