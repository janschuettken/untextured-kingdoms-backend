//! Wie ein Computergegner spielt: Bauliste, Armee, Mauer, Wege, Handel.
//!
//! Alles, was eine Stärke ausmacht, steht hier als Daten. Eine neue Stufe oder
//! Strategie ist ein neues `Profile`, der Ablauf in `mod.rs` bleibt gleich.

use protocol::{AiLevel, AiPersonality, BuildingKind, CastleLayout, Good, UnitKind};

use crate::economy::Economy;

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
    /// Baut er überhaupt einen Mauerring (der Charakter kann ihn abschalten)?
    pub walls: bool,
    /// Grundriss der Burg (nie `Level`).
    pub castle: CastleLayout,
    /// Rezepte, die er laufen lässt, sobald die Gebäudestufe sie freischaltet. Kann ein
    /// Betrieb mehrere davon, wechselt er zu dem, wovon am wenigsten im Vorrat liegt.
    pub recipes: Vec<String>,
    /// Angriffe, `None` = greift nie an.
    pub attack: Option<Attack>,
    /// Angriff trotz Unterlegenheit, 0 bis 1 (1 = ständig, sobald eine Welle bereit ist).
    pub daring: f32,
    /// Hilfsbereitschaft gegenüber Verbündeten, 0 bis 1 (Anteil der freien Soldaten).
    pub helpfulness: f32,
}

/// Wann und wie er angreift.
#[derive(Debug, Clone)]
pub struct Attack {
    /// Frühestens nach so vielen Spielminuten.
    pub first_minutes: f32,
    /// So viele Soldaten braucht eine Welle mindestens.
    pub wave_min: u32,
    /// So viele Soldaten bleiben immer daheim (dazu alle auf Mauern und Türmen).
    pub home_guard: u32,
    /// Die Welle muss so viel stärker sein als der geschätzte Gegner (1.0 = gleich stark).
    pub superiority: f32,
    /// Nach einer Welle so viele Sekunden Pause.
    pub cooldown_seconds: f32,
    /// So lange bleibt die Welle beim Gegner, dann geht sie heim (ohne Kampf kommt sie
    /// nicht weiter; mit Kampf kämpft sie dort).
    pub siege_seconds: f32,
    /// Geht heim, wenn weniger als dieser Anteil der Welle übrig ist.
    pub retreat_below: f32,
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
                army_size: 8,
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
                walls: true,
                castle: CastleLayout::Square,
                recipes: Vec::new(),
                daring: 0.3,
                helpfulness: 0.5,
                attack: Some(Attack {
                    first_minutes: 25.0,
                    wave_min: 5,
                    home_guard: 2,
                    superiority: 1.6,
                    cooldown_seconds: 360.0,
                    siege_seconds: 120.0,
                    retreat_below: 0.4,
                }),
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
                walls: true,
                castle: CastleLayout::Octagon,
                recipes: Vec::new(),
                daring: 0.3,
                helpfulness: 0.5,
                attack: Some(Attack {
                    first_minutes: 18.0,
                    wave_min: 8,
                    home_guard: 4,
                    superiority: 1.3,
                    cooldown_seconds: 240.0,
                    siege_seconds: 150.0,
                    retreat_below: 0.35,
                }),
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
                walls: true,
                castle: CastleLayout::Narrows,
                recipes: Vec::new(),
                daring: 0.3,
                helpfulness: 0.5,
                // Große Wellen: wartet, bis viele beisammen sind, und greift dann an.
                attack: Some(Attack {
                    first_minutes: 14.0,
                    wave_min: 12,
                    home_guard: 4,
                    superiority: 1.1,
                    cooldown_seconds: 180.0,
                    siege_seconds: 180.0,
                    retreat_below: 0.3,
                }),
            },
        }
    }
}

/// Gewicht in Prozent aus dem Charakter (fehlt = 100).
fn weight<K: Ord>(map: &std::collections::BTreeMap<K, u32>, k: &K) -> u32 {
    map.get(k).copied().unwrap_or(100)
}

/// Aus einer Reihe gleichartiger Gebäude (schwach bis stark) das passende: das der Stufe,
/// außer der Charakter will eins mit mindestens 200 % (dann das stärkste davon) oder
/// verbietet das der Stufe (dann das nächste erlaubte). `None` = alle verboten.
fn pick(p: &AiPersonality, family: &[BuildingKind], level_kind: BuildingKind) -> Option<BuildingKind> {
    let w = |k: &BuildingKind| weight(&p.buildings, k);
    if let Some(&strong) = family.iter().rev().find(|k| w(k) >= 200) {
        return Some(strong);
    }
    if w(&level_kind) > 0 {
        return Some(level_kind);
    }
    let at = family.iter().position(|&k| k == level_kind).unwrap_or(0);
    // Erst schwächere, dann stärkere.
    (0..family.len())
        .flat_map(|d| [at.checked_sub(d), Some(at + d)])
        .flatten()
        .filter_map(|i| family.get(i))
        .find(|k| w(k) > 0)
        .copied()
}

impl Profile {
    /// Profil der Stufe mit dem Charakter darauf. `economy` braucht er, um zu wissen, was
    /// zusätzliche Soldaten brauchen (Waffen, Betriebe, Kasernenstufe).
    /// Profil für einen Platz: Stufe, Charakter und die Regler aus der Lobby.
    pub fn for_seat(seat: &crate::ai::Seat, all: &protocol::AiPersonalities, economy: &Economy) -> Profile {
        let mut prof = Profile::build(seat.level, crate::ai::personality::find(all, &seat.personality), economy);
        if let Some(d) = seat.daring {
            prof.daring = d.min(100) as f32 / 100.0;
        }
        if let Some(h) = seat.helpfulness {
            prof.helpfulness = h.min(100) as f32 / 100.0;
        }
        prof
    }

    pub fn build(level: AiLevel, p: &AiPersonality, economy: &Economy) -> Profile {
        let mut prof = Profile::for_level(level);
        let bw = |k: BuildingKind| weight(&p.buildings, &k);

        // Bauliste: Anzahl skalieren, 0 streichen.
        prof.build_order = prof
            .build_order
            .iter()
            .filter_map(|&(k, n)| {
                let w = bw(k);
                (w > 0).then(|| (k, ((n * w + 50) / 100).max(1)))
            })
            .collect();
        // Was die Stufe nicht baut, ab 200 % dazu.
        for (&k, &w) in &p.buildings {
            let fortification = economy.building(k).is_some_and(|d| d.category == protocol::BuildingCategory::Fortification);
            let road = economy.building(k).is_some_and(|d| d.category == protocol::BuildingCategory::Road);
            if w >= 200 && !fortification && !road && !prof.build_order.iter().any(|e| e.0 == k) {
                prof.build_order.push((k, (w - 100) / 100));
            }
        }
        prof.upgrades.retain(|&(k, _)| bw(k) > 0);

        // Mauer, Tor, Türme, Wege.
        match pick(p, &[Palisade, Wall, HighWall], prof.wall_kind) {
            Some(k) => prof.wall_kind = k,
            None => prof.walls = false,
        }
        if bw(Gate) == 0 {
            prof.walls = false;
        }
        prof.main_gate = pick(p, &[Gate, Gatehouse], prof.main_gate).unwrap_or(Gate);
        prof.corner_towers = bw(Tower) > 0 && (prof.corner_towers || bw(Tower) >= 200);
        prof.road_kind = prof.road_kind.and_then(|k| pick(p, &[WoodPath, StonePath], k));
        if bw(Market) == 0 {
            prof.trade = None;
        }
        // Mauerausbau auf eine andere Mauerart umhängen.
        let wall = prof.wall_kind;
        for u in &mut prof.upgrades {
            if matches!(u.0, Palisade | Wall | HighWall) {
                u.0 = wall;
            }
        }

        // Soldaten: wie oft je Art. Die der Stufe zählen mit 100 %.
        let mut weights: Vec<(UnitKind, u32)> = Vec::new();
        for &u in &prof.army_units {
            match weights.iter_mut().find(|e| e.0 == u) {
                Some(e) => e.1 += 100,
                None => weights.push((u, 100)),
            }
        }
        for e in &mut weights {
            if let Some(&w) = p.units.get(&e.0) {
                // Kommt er in der Stufe mehrfach vor, bleibt das Verhältnis.
                e.1 = e.1 / 100 * w;
            }
        }
        for (&u, &w) in &p.units {
            if w > 0 && !weights.iter().any(|e| e.0 == u) {
                weights.push((u, w));
                prof.add_needs(economy, u);
            }
        }
        weights.retain(|e| e.1 > 0);
        if !weights.is_empty() {
            prof.army_units = interleave(&weights);
        }
        prof.army_size = (prof.army_size * p.army + 50) / 100;

        if p.castle != CastleLayout::Level {
            prof.castle = p.castle;
        }
        prof.daring = p.daring.min(100) as f32 / 100.0;
        prof.helpfulness = p.helpfulness.min(100) as f32 / 100.0;
        // Angriffslust.
        if p.aggression == 0 {
            prof.attack = None;
        }
        if let Some(a) = &mut prof.attack {
            let f = p.aggression as f32 / 100.0;
            a.superiority /= f.sqrt();
            a.cooldown_seconds /= f;
            a.first_minutes /= f.sqrt();
            a.wave_min = ((a.wave_min as f32 * p.army as f32 / 100.0).round() as u32).max(3);
        }
        prof
    }

    /// Was ein zusätzlicher Soldat braucht: Kasernenstufe, Betriebe für seine Ausrüstung
    /// (rückwärts durch die Rezepte bis zum Rohstoff), deren Stufe und Rezept.
    fn add_needs(&mut self, economy: &Economy, unit: UnitKind) {
        let Some(def) = economy.unit(unit) else { return };
        let mut needs: Vec<(BuildingKind, u32)> = Vec::new();
        for b in &economy.def.buildings {
            if let Some(i) = b.levels.iter().position(|l| l.trains.contains(&unit)) {
                needs.push((b.kind, i as u32 + 1));
                break;
            }
        }
        for c in &def.cost {
            self.good_needs(economy, c.good, &mut needs, 0);
        }
        // Ritter kommen beritten aus der Kaserne: dafür eine Pferdezucht.
        if def.mounted {
            self.good_needs(economy, Good::Horse, &mut needs, 0);
        }
        for (kind, level) in needs {
            if !self.build_order.iter().any(|e| e.0 == kind) {
                self.build_order.push((kind, 1));
            }
            if level > 1 {
                match self.upgrades.iter_mut().find(|u| u.0 == kind) {
                    Some(u) => u.1 = u.1.max(level),
                    None => self.upgrades.push((kind, level)),
                }
            }
        }
    }

    fn good_needs(&mut self, economy: &Economy, good: Good, needs: &mut Vec<(BuildingKind, u32)>, depth: u32) {
        if depth > 6 {
            return;
        }
        // Erster Betrieb (in Dateireihenfolge), der die Ware herstellt, und auf welcher Stufe.
        let found = economy.def.buildings.iter().find_map(|b| {
            b.levels.iter().enumerate().find_map(|(i, l)| {
                l.recipes.iter().find_map(|id| {
                    let r = economy.recipe(id)?;
                    r.outputs.iter().any(|o| o.good == good).then(|| (b.kind, i as u32 + 1, r.clone()))
                })
            })
        });
        let Some((kind, level, recipe)) = found else { return };
        if !needs.iter().any(|n| n.0 == kind && n.1 >= level) {
            needs.retain(|n| n.0 != kind);
            needs.push((kind, level));
        }
        if !self.recipes.contains(&recipe.id) {
            self.recipes.push(recipe.id.clone());
        }
        for i in &recipe.inputs {
            self.good_needs(economy, i.good, needs, depth + 1);
        }
    }
}

/// Reihenfolge der Ausbildung nach Gewichten, gut gemischt: 300/100 ergibt A A B A ...
fn interleave(weights: &[(UnitKind, u32)]) -> Vec<UnitKind> {
    let min = weights.iter().map(|e| e.1).min().unwrap_or(100).max(1);
    let counts: Vec<(UnitKind, u32)> = weights.iter().map(|&(u, w)| (u, ((w as f32 / min as f32).round() as u32).clamp(1, 8))).collect();
    let total: u32 = counts.iter().map(|c| c.1).sum();
    let mut done = vec![0u32; counts.len()];
    let mut out = Vec::new();
    for step in 1..=total {
        // Wer am meisten hinter seinem Anteil liegt.
        let i = (0..counts.len())
            .max_by(|&a, &b| {
                let lag = |i: usize| counts[i].1 as f32 * step as f32 / total as f32 - done[i] as f32;
                lag(a).total_cmp(&lag(b)).then(b.cmp(&a))
            })
            .unwrap();
        done[i] += 1;
        out.push(counts[i].0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::personality;

    fn eco() -> Economy {
        Economy::parse(crate::economy::ECONOMY_JSON).unwrap()
    }

    #[test]
    fn ritter_brauchen_eine_pferdezucht() {
        let mut p = personality::neutral();
        p.units.insert(UnitKind::Knight, 100);
        let prof = Profile::build(AiLevel::Hard, &p, &eco());
        assert!(prof.build_order.iter().any(|e| e.0 == HorseFarm), "{:?}", prof.build_order);
        assert!(prof.recipes.contains(&"breed_horse".to_string()));
    }

    #[test]
    fn neutraler_charakter_aendert_nichts() {
        let a = Profile::for_level(AiLevel::Normal);
        let b = Profile::build(AiLevel::Normal, &personality::neutral(), &eco());
        assert_eq!(a.build_order, b.build_order);
        assert_eq!(a.army_units, b.army_units);
        assert_eq!(a.army_size, b.army_size);
    }

    #[test]
    fn charakter_verschiebt_bauliste_und_soldaten() {
        let mut p = personality::neutral();
        p.buildings.insert(Market, 0);
        p.buildings.insert(House, 200);
        p.buildings.insert(HighWall, 200);
        p.units.insert(UnitKind::Spearman, 0);
        p.units.insert(UnitKind::Crossbowman, 100);
        p.aggression = 0;
        let prof = Profile::build(AiLevel::Hard, &p, &eco());
        assert!(!prof.build_order.iter().any(|e| e.0 == Market));
        assert!(prof.trade.is_none());
        assert_eq!(prof.build_order.iter().filter(|e| e.0 == House).map(|e| e.1).max(), Some(8));
        assert!(!prof.army_units.contains(&UnitKind::Spearman));
        assert!(prof.army_units.contains(&UnitKind::Crossbowman));
        // Armbrust: Bogner Stufe 2 mit Eisen (Schmelze, Mine), Rüstung aus Leder, Kaserne Stufe 2.
        assert!(prof.upgrades.contains(&(Bowyer, 2)), "{:?}", prof.upgrades);
        assert!(prof.upgrades.iter().any(|u| u.0 == Barracks && u.1 >= 2));
        for k in [Smelter, Mine, Tannery] {
            assert!(prof.build_order.iter().any(|e| e.0 == k), "{k:?} fehlt: {:?}", prof.build_order);
        }
        assert!(prof.recipes.contains(&"make_crossbow".to_string()));
        assert!(prof.attack.is_none());
        assert_eq!(prof.wall_kind, HighWall);
    }

    #[test]
    fn ohne_mauer_und_mit_anderem_weg() {
        let mut p = personality::neutral();
        for k in [Palisade, Wall, HighWall] {
            p.buildings.insert(k, 0);
        }
        p.buildings.insert(WoodPath, 0);
        let prof = Profile::build(AiLevel::Easy, &p, &eco());
        assert!(!prof.walls);
        assert_eq!(prof.road_kind, Some(StonePath));
    }

    #[test]
    fn mischung_nach_gewicht() {
        let order = interleave(&[(UnitKind::Archer, 300), (UnitKind::Spearman, 100)]);
        assert_eq!(order.iter().filter(|&&u| u == UnitKind::Archer).count(), 3);
        assert_eq!(order.len(), 4);
    }
}
