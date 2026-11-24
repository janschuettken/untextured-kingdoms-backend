//! Wirtschaft als Daten: Waren, Gebäude mit Stufen und Kosten, Rezepte.
//!
//! Alles steht in `server/data/economy.json` (beim Kompilieren eingebettet).
//! Die Datei geht unverändert im `welcome` an den Client, damit Baumenü,
//! Kosten und später die Übersicht der Produktionsketten daraus entstehen.
//! Balancing heißt also: nur diese Datei ändern.

use std::collections::HashMap;

use protocol::{BuildingDef, BuildingKind, Damage, DamageType, EconomyDef, Good, GoodAmount, RecipeDef, ResourceKind, UnitDef, UnitKind};

/// Kampfwerte des Königs, wenn die Balancing-Datei keine hat. Sein Tempo kommt aus `movement.king_speed`.
fn default_king(speed: f32) -> UnitDef {
    UnitDef {
        kind: UnitKind::King,
        cost: Vec::new(),
        taler: 0,
        seconds: 0.0,
        speed,
        hp: 300,
        attack: 14,
        damage: Vec::new(),
        // Nur Nahkämpfer können ihn töten.
        armor: vec![
            protocol::Armor { kind: DamageType::Ranged, min: 0, block: 1.0 },
            protocol::Armor { kind: DamageType::HeavyRanged, min: 0, block: 1.0 },
        ],
        horse_armor: Vec::new(),
        range: 1.2,
        upkeep: 0,
        food: 0,
        can_ride: false,
        mounted: false,
        attack_standing: true,
        attack_moving: false,
        attack_mounted: true,
        attack_seconds: 1.5,
        heal: 0,
        heal_range: 0.0,
        siege: None,
    }
}

/// Was eine ältere Balancing-Datei noch nicht kennt (neue Waren, neue Gebäude samt
/// ihren Rezepten), kommt aus den eingebauten Werten dazu. Vorhandene Werte bleiben.
fn fill_from_builtin(def: &mut EconomyDef) {
    let Some(builtin) = builtin_def() else { return };
    for g in &builtin.goods {
        if !def.goods.contains(g) {
            def.goods.push(*g);
        }
    }
    for b in &builtin.buildings {
        if def.buildings.iter().any(|x| x.kind == b.kind) {
            continue;
        }
        for id in b.levels.iter().flat_map(|l| &l.recipes) {
            if !def.recipes.iter().any(|r| &r.id == id) {
                if let Some(r) = builtin.recipes.iter().find(|r| &r.id == id) {
                    def.recipes.push(r.clone());
                }
            }
        }
        def.buildings.push(b.clone());
    }
    // Neue Einheiten (z. B. Baumeister, Belagerungsgeräte).
    for u in &builtin.units {
        if !def.units.iter().any(|x| x.kind == u.kind) {
            def.units.push(u.clone());
        }
    }
}

/// Die eingebaute economy.json, roh gelesen (ohne Ergänzungen), aber wie `Economy::parse`
/// über `migrate`: so stört weder ein älteres Format noch ein doppelter Schlüssel.
fn builtin_def() -> Option<EconomyDef> {
    let mut value: serde_json::Value = serde_json::from_str(ECONOMY_JSON).ok()?;
    migrate(&mut value);
    serde_json::from_value(value).ok()
}

/// Aktueller Aufbau der Balancing-Datei.
/// Format 3: Schadensarten und Rüstung (`damage`, `armor`, `horse_armor`, Stufe `armor`).
/// Format 4: `build_radius` einmal je Gebäude statt je Stufe.
pub const FORMAT: u32 = 4;

/// Werte, die je Gebäudestufe gelten. Fehlen sie in einer Stufe, gilt die Stufe davor.
const LEVEL_FIELDS: [&str; 14] = [
    "max_hp", "workers", "gather_radius", "service_radius", "walkway", "stairs", "gate_radius", "garrison",
    "cover_percent", "road_speed_percent", "hub_capacity", "storage_inputs", "storage_outputs", "armor",
];

/// Bringt ältere Balancing-Dateien in den aktuellen Aufbau (Format 2: Werte je Stufe).
///
/// Format 1 hatte Lebenspunkte, Arbeiter, Reichweiten usw. einmal je Gebäude, Wehrgang und
/// Treppe als „ab Stufe“ und einen Angriffstakt für alle Einheiten (`army.attack_seconds`).
/// Beim Umwandeln bekommt jede Stufe die alten Werte, Wehrgang/Treppe gelten ab ihrer Stufe,
/// und jede Einheit bekommt den alten Angriffstakt. Alles andere bleibt, wie es war.
pub fn migrate(v: &mut serde_json::Value) {
    use serde_json::Value;
    let Some(root) = v.as_object_mut() else { return };
    let army_attack = root
        .get_mut("army")
        .and_then(Value::as_object_mut)
        .and_then(|a| a.remove("attack_seconds"))
        .and_then(|x| x.as_f64());
    if let Some(units) = root.get_mut("units").and_then(Value::as_array_mut) {
        for u in units.iter_mut().filter_map(Value::as_object_mut) {
            let missing = u.get("attack_seconds").and_then(Value::as_f64).is_none_or(|x| x <= 0.0);
            if missing {
                u.insert("attack_seconds".into(), Value::from(army_attack.unwrap_or(1.5)));
            }
            // Format 2: `ranged_immune` (König) wird zur Rüstung, die Fernkampf ganz abwehrt.
            // Ausdrücklich nicht immun: neutrale Einträge, damit nicht die eingebaute Rüstung kommt.
            let immune = u.remove("ranged_immune").and_then(|x| x.as_bool());
            if let Some(immune) = immune.filter(|_| !u.contains_key("armor")) {
                let king = u.get("kind").and_then(Value::as_str) == Some("king");
                let block = if immune { 1 } else { 0 };
                if immune || king {
                    let entry = |t: &str| serde_json::json!({"type": t, "block": block});
                    u.insert("armor".into(), Value::Array(vec![entry("ranged"), entry("heavy_ranged")]));
                }
            }
        }
    }
    if let Some(buildings) = root.get_mut("buildings").and_then(Value::as_array_mut) {
        for b in buildings.iter_mut().filter_map(Value::as_object_mut) {
            let mut moved = serde_json::Map::new();
            for k in LEVEL_FIELDS {
                if let Some(x) = b.remove(k) {
                    moved.insert(k.into(), x);
                }
            }
            let from = |b: &mut serde_json::Map<String, Value>, k: &str| b.remove(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let walk_from = from(b, "walkway_from_level");
            let stairs_from = from(b, "stairs_from_level");
            let base_spawns = b.remove("spawns").and_then(|x| x.as_array().cloned()).unwrap_or_default();
            // Format 3 und älter: `build_radius` je Stufe. Jetzt einmal je Gebäude, der größte Wert gilt.
            let level_radius = b
                .get_mut("levels")
                .and_then(Value::as_array_mut)
                .map(|ls| {
                    ls.iter_mut()
                        .filter_map(|l| l.as_object_mut()?.remove("build_radius")?.as_u64())
                        .max()
                })
                .unwrap_or(None);
            if let Some(r) = level_radius.filter(|_| !b.contains_key("build_radius")) {
                b.insert("build_radius".into(), Value::from(r));
            }
            let Some(levels) = b.get_mut("levels").and_then(Value::as_array_mut) else { continue };
            for (i, l) in levels.iter_mut().enumerate() {
                let Some(l) = l.as_object_mut() else { continue };
                let n = i as u64 + 1;
                for (k, x) in &moved {
                    // Format 1: `hub_capacity` 0 in einer Stufe hieß „wie beim Gebäude“.
                    if k == "hub_capacity" && l.get(k).and_then(Value::as_u64) == Some(0) {
                        l.remove(k);
                    }
                    l.entry(k.clone()).or_insert_with(|| x.clone());
                }
                if walk_from > 0 && n >= walk_from {
                    l.entry("walkway").or_insert(Value::Bool(true));
                }
                if stairs_from > 0 && n >= stairs_from {
                    l.entry("stairs").or_insert(Value::Bool(true));
                }
                if i == 0 && !base_spawns.is_empty() {
                    let mut all = base_spawns.clone();
                    if let Some(own) = l.get("spawns").and_then(Value::as_array) {
                        all.extend(own.iter().cloned());
                    }
                    l.insert("spawns".into(), Value::Array(all));
                }
            }
            // Fehlende Werte erbt eine Stufe von der davor.
            for i in 1..levels.len() {
                let prev = levels[i - 1].clone();
                let Some(l) = levels[i].as_object_mut() else { continue };
                for k in LEVEL_FIELDS {
                    if let Some(x) = prev.get(k) {
                        l.entry(k).or_insert_with(|| x.clone());
                    }
                }
            }
        }
    }
    root.insert("format".into(), Value::from(FORMAT));
}

/// Grundhöhe eines Gebäudes (Welt) bis Oberkante samt Zinnen bzw. Dach.
fn default_height(kind: protocol::BuildingKind) -> f32 {
    use protocol::BuildingKind as K;
    match kind {
        K::Wall | K::Palisade | K::Staircase => 1.5,
        K::HighWall => 2.4,
        K::Gate => 2.6,
        K::Gatehouse => 3.6,
        K::Tower => 3.7,
        K::HighTower => 6.6,
        K::Keep => 5.2,
        K::WoodPath | K::StonePath | K::Bridge => 0.0,
        K::Farm | K::Orchard | K::PigFarm | K::CattleFarm => 1.0,
        _ => 1.8,
    }
}

pub const ECONOMY_JSON: &str = include_str!("../data/economy.json");

#[derive(Clone)]
pub struct Economy {
    pub def: EconomyDef,
    buildings: HashMap<BuildingKind, usize>,
}

impl Economy {
    /// Für Tests, die nichts mit Bewohnern zu tun haben: niemand braucht eine Wohnung.
    #[cfg(test)]
    pub fn without_people(mut self) -> Economy {
        for b in &mut self.def.buildings {
            b.levels.iter_mut().for_each(|l| l.workers = 0);
        }
        self.def.population.per_transport = 0;
        self
    }

    /// Für Tests, die Mauern ohne Bergfried bauen: überall bauen erlaubt.
    #[cfg(test)]
    pub fn without_build_radius(mut self) -> Economy {
        for b in &mut self.def.buildings {
            b.build_radius = 0;
        }
        self
    }

    /// Für Tests, die ihre Gebäude selbst setzen: kein Bergfried an der Fahne.
    #[cfg(test)]
    pub fn without_start_building(mut self) -> Economy {
        self.def.start_building = None;
        self
    }

    /// Liest und prüft die Wirtschaftsdaten.
    pub fn parse(json: &str) -> Result<Economy, String> {
        let mut value: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("economy.json: {e}"))?;
        let old_format = value.get("format").and_then(serde_json::Value::as_u64).unwrap_or(1) < 3;
        migrate(&mut value);
        let mut def: EconomyDef = serde_json::from_value(value).map_err(|e| format!("economy.json: {e}"))?;
        // Ältere Balancing-Dateien kennen den König noch nicht: Grundwerte einsetzen.
        if !def.units.iter().any(|u| u.kind == UnitKind::King) {
            def.units.insert(0, default_king(def.movement.king_speed));
        }
        // Höhe für Pfeile: fehlt sie, Grundwert je Art (passend zu den Modellen im Client).
        for b in &mut def.buildings {
            if b.height <= 0.0 && !b.is_road() {
                b.height = default_height(b.kind);
            }
        }
        // Zivilisten ohne Werte: Grundwerte (nur Lebenspunkte).
        for (kind, hp) in [(UnitKind::Worker, 30), (UnitKind::Carrier, 30), (UnitKind::Cart, 60), (UnitKind::TradeCart, 60)] {
            if !def.units.iter().any(|u| u.kind == kind) {
                def.units.push(UnitDef { kind, hp, armor: Vec::new(), attack: 0, range: 0.0, ..default_king(1.0) });
            }
        }
        add_apothecary(&mut def);
        fill_from_builtin(&mut def);
        if old_format {
            add_armor_from_builtin(&mut def);
        }
        fill_damage(&mut def);
        let mut buildings = HashMap::new();
        for (i, b) in def.buildings.iter().enumerate() {
            if buildings.insert(b.kind, i).is_some() {
                return Err(format!("Gebäude {:?} doppelt", b.kind));
            }
            if b.levels.is_empty() || b.w == 0 || b.h == 0 {
                return Err(format!("Gebäude {:?} braucht Größe und mindestens eine Stufe", b.kind));
            }
            if let Some(n) = b.levels.iter().position(|l| l.max_hp == 0) {
                return Err(format!("Gebäude {:?} Stufe {}: braucht Lebenspunkte", b.kind, n + 1));
            }
            if b.levels.iter().flat_map(|l| &l.spawns).any(|s| !matches!(s.kind, UnitKind::Carrier | UnitKind::Cart)) {
                return Err(format!("Gebäude {:?}: nur Träger und Karren gehören zu einem Gebäude", b.kind));
            }
        }
        let mut ids = HashMap::new();
        for r in &def.recipes {
            if ids.insert(r.id.as_str(), r).is_some() {
                return Err(format!("Rezept {} doppelt", r.id));
            }
            if !buildings.contains_key(&r.building) {
                return Err(format!("Rezept {}: Gebäude {:?} fehlt", r.id, r.building));
            }
        }
        for b in &def.buildings {
            for (n, level) in b.levels.iter().enumerate() {
                for id in &level.recipes {
                    match ids.get(id.as_str()) {
                        Some(r) if r.building == b.kind => {}
                        Some(_) => return Err(format!("{:?} Stufe {}: Rezept {id} gehört zu einem anderen Gebäude", b.kind, n + 1)),
                        None => return Err(format!("{:?} Stufe {}: Rezept {id} fehlt", b.kind, n + 1)),
                    }
                }
            }
        }
        if let Some(r) = def.recipes.iter().find(|r| !(r.seconds > 0.0 && r.seconds.is_finite())) {
            return Err(format!("Rezept {}: Dauer muss größer als 0 sein", r.id));
        }
        if let Some(r) = def.recipes.iter().find(|r| r.outputs.is_empty()) {
            return Err(format!("Rezept {}: braucht mindestens ein Ergebnis", r.id));
        }
        let t = &def.transport;
        if t.carrier_capacity == 0 || t.cart_capacity == 0 || t.building_capacity == 0 {
            return Err("Transport: Kapazitäten müssen größer als 0 sein".into());
        }
        if !(t.carrier_speed > 0.0 && t.cart_speed > 0.0 && t.carrier_speed.is_finite() && t.cart_speed.is_finite()) {
            return Err("Transport: Geschwindigkeiten müssen größer als 0 sein".into());
        }
        if !(def.movement.king_speed > 0.0 && def.movement.king_speed.is_finite()) {
            return Err("König: Geschwindigkeit muss größer als 0 sein".into());
        }
        let mv = &def.movement;
        if !(mv.replan_delay_seconds >= 0.0 && mv.replan_delay_seconds <= 60.0) {
            return Err("movement.replan_delay_seconds: 0 bis 60".into());
        }
        if mv.walkway_step_levels > 9 {
            return Err("movement.walkway_step_levels: 0 bis 9".into());
        }
        if !(mv.spread_distance >= 0.0 && mv.spread_distance <= 3.0 && mv.spread_speed >= 0.0 && mv.spread_speed.is_finite()) {
            return Err("Auseinanderrücken: Abstand 0 bis 3, Tempo nicht negativ".into());
        }
        if def.base_capacity == 0 {
            return Err("base_capacity muss größer als 0 sein".into());
        }
        let pop = &def.population;
        if !(pop.meal_seconds >= 0.0 && pop.meal_seconds.is_finite()) {
            return Err("Bewohner: meal_seconds darf nicht negativ sein (0 = essen nie)".into());
        }
        if !(1..=100).contains(&pop.hungry_speed_percent) {
            return Err("Bewohner: hungry_speed_percent muss zwischen 1 und 100 liegen".into());
        }
        if pop.rations.iter().any(|r| r.food == 0 || r.speed_percent <= -100) {
            return Err("Bewohner: jede Ration braucht mindestens 1 Essen, Tempo über -100 %".into());
        }
        if !pop.rations.is_empty() && pop.default_ration as usize >= pop.rations.len() {
            return Err("Bewohner: default_ration zeigt auf keine Ration".into());
        }
        if def.market.tax_levels.iter().any(|t| t.speed_percent <= -100) {
            return Err("Markt: Steuersatz mit Tempo -100 % oder weniger".into());
        }
        if !def.market.tax_levels.is_empty() && def.market.default_tax as usize >= def.market.tax_levels.len() {
            return Err("Markt: default_tax zeigt auf keinen Steuersatz".into());
        }
        if !pop.food.iter().all(|g| def.goods.contains(g)) {
            return Err("Bewohner: ein Essen steht nicht in goods".into());
        }
        for (i, p) in def.market.prices.iter().enumerate() {
            if !def.goods.contains(&p.good) {
                return Err("Markt: ein Preis gehört zu einer Ware, die nicht in goods steht".into());
            }
            if def.market.prices[..i].iter().any(|q| q.good == p.good) {
                return Err("Markt: eine Ware hat zwei Preise".into());
            }
            if p.sell > p.buy {
                return Err("Markt: Verkaufspreis darf nicht über dem Kaufpreis liegen (sonst gibt es Geld aus dem Nichts)".into());
            }
        }
        for (i, u) in def.units.iter().enumerate() {
            if !u.kind.is_soldier() && u.kind != UnitKind::King && !u.kind.is_civilian() && !u.kind.is_siege() {
                return Err(format!("Einheit {:?}: hat keine Kampfwerte", u.kind));
            }
            if def.units[..i].iter().any(|v| v.kind == u.kind) {
                return Err(format!("Einheit {:?} doppelt", u.kind));
            }
            if u.kind == UnitKind::King || u.kind.is_civilian() {
                if u.hp == 0 {
                    return Err(format!("Einheit {:?}: braucht Lebenspunkte", u.kind));
                }
                continue;
            }
            // Belagerungsgeräte dürfen feststehen (Tempo 0); `seconds` ist dort die Bauzeit.
            let speed_ok = if u.kind.is_siege() { u.speed >= 0.0 } else { u.speed > 0.0 };
            if u.kind.is_siege() != u.siege.is_some() {
                return Err(format!("Einheit {:?}: nur Belagerungsgeräte haben „siege“", u.kind));
            }
            if !(u.seconds > 0.0 && u.seconds.is_finite() && speed_ok && u.speed.is_finite()) {
                return Err(format!("Einheit {:?}: Ausbildungszeit und Tempo müssen größer als 0 sein", u.kind));
            }
            if u.hp == 0 {
                return Err(format!("Einheit {:?}: braucht Lebenspunkte", u.kind));
            }
            if !(u.attack_seconds > 0.0 && u.attack_seconds.is_finite()) {
                return Err(format!("Einheit {:?}: Sekunden zwischen zwei Angriffen müssen größer als 0 sein", u.kind));
            }
        }
        let armor_ok = |a: &[protocol::Armor]| {
            a.iter().enumerate().all(|(i, x)| (0.0..=1.0).contains(&x.block) && !a[..i].iter().any(|y| y.kind == x.kind))
        };
        for u in &def.units {
            if !(armor_ok(&u.armor) && armor_ok(&u.horse_armor)) {
                return Err(format!("Einheit {:?}: Rüstung braucht Abwehr zwischen 0 und 1, jede Schadensart nur einmal", u.kind));
            }
            if u.damage.iter().enumerate().any(|(i, d)| u.damage[..i].iter().any(|e| e.kind == d.kind)) {
                return Err(format!("Einheit {:?}: jede Schadensart nur einmal", u.kind));
            }
        }
        for b in &def.buildings {
            if let Some(n) = b.levels.iter().position(|l| !armor_ok(&l.armor)) {
                return Err(format!("Gebäude {:?} Stufe {}: Rüstung braucht Abwehr zwischen 0 und 1, jede Schadensart nur einmal", b.kind, n + 1));
            }
        }
        for b in &def.buildings {
            for (n, level) in b.levels.iter().enumerate() {
                if let Some(k) = level.trains.iter().find(|k| !k.is_soldier()) {
                    return Err(format!("{:?} Stufe {}: {k:?} wird nicht ausgebildet", b.kind, n + 1));
                }
                if let Some(k) = level.trains.iter().find(|k| !def.units.iter().any(|u| u.kind == **k)) {
                    return Err(format!("{:?} Stufe {}: Einheit {k:?} fehlt unter units", b.kind, n + 1));
                }
            }
        }
        let army = &def.army;
        if !(army.upkeep_seconds >= 0.0 && army.upkeep_seconds.is_finite()) {
            return Err("Heer: upkeep_seconds darf nicht negativ sein (0 = kein Sold)".into());
        }
        if !(1..=100).contains(&army.unpaid_speed_percent) {
            return Err("Heer: unpaid_speed_percent muss zwischen 1 und 100 liegen".into());
        }
        if army.queue_max == 0 {
            return Err("Heer: queue_max muss mindestens 1 sein".into());
        }
        if !(army.melee_max_levels >= 0.0 && army.melee_max_levels.is_finite() && army.height_range_percent >= 0.0 && army.height_range_percent <= 100.0) {
            return Err("Heer: melee_max_levels und height_range_percent (0 bis 100) dürfen nicht negativ sein".into());
        }
        if !(army.arrow_speed > 0.0 && army.arrow_speed.is_finite() && army.arrow_hit_radius >= 0.0 && army.arrow_hit_radius.is_finite()) {
            return Err("Heer: arrow_speed muss über 0 liegen, arrow_hit_radius darf nicht negativ sein".into());
        }
        if army.loot_percent > 100 {
            return Err("Heer: loot_percent höchstens 100".into());
        }
        if !(army.melee_range >= 0.0 && army.melee_range.is_finite()) {
            return Err("Heer: melee_range darf nicht negativ sein".into());
        }
        if army.horse_hp == 0 {
            return Err("Heer: horse_hp muss mindestens 1 sein".into());
        }
        if !(army.horse_speed > 0.0 && army.horse_speed.is_finite()) {
            return Err("Heer: horse_speed muss größer als 0 sein".into());
        }
        if !(army.fall_seconds >= 0.0 && army.fall_seconds.is_finite()) {
            return Err("Heer: fall_seconds darf nicht negativ sein".into());
        }
        if !(army.alert_radius >= 0.0 && army.alert_radius.is_finite()) {
            return Err("Heer: alert_radius darf nicht negativ sein (0 = aus)".into());
        }
        if !(army.alert_seconds >= 0.0 && army.alert_seconds.is_finite()) {
            return Err("Heer: alert_seconds darf nicht negativ sein".into());
        }
        if !(army.enemy_block_radius >= 0.0 && army.enemy_block_radius.is_finite()) {
            return Err("Heer: enemy_block_radius darf nicht negativ sein (0 = aus)".into());
        }
        if !(army.heal_seconds > 0.0 && army.heal_seconds.is_finite()) {
            return Err("Heer: heal_seconds muss größer als 0 sein".into());
        }
        if let Some(u) = def.units.iter().find(|u| !(u.heal_range >= 0.0 && u.heal_range.is_finite())) {
            return Err(format!("Einheit {:?}: heal_range darf nicht negativ sein", u.kind));
        }
        if !(army.civilian_respawn_seconds >= 0.0 && army.civilian_respawn_seconds.is_finite()) {
            return Err("Heer: civilian_respawn_seconds darf nicht negativ sein (0 = kein Ersatz)".into());
        }
        let goods = |list: &[GoodAmount]| list.iter().all(|g| def.goods.contains(&g.good));
        if !def.buildings.iter().all(|b| b.levels.iter().all(|l| goods(&l.cost)))
            || !def.recipes.iter().all(|r| goods(&r.inputs) && goods(&r.outputs))
            || !goods(&def.start_stock)
            || !def.units.iter().all(|u| goods(&u.cost))
        {
            return Err("Eine Ware wird benutzt, steht aber nicht in goods".into());
        }
        Ok(Economy { def, buildings })
    }

    /// Wirtschaftsdaten aus einer Datei. Fehlt sie, gelten die eingebauten Werte.
    /// Gebäude und Einheiten, die es in der Datei noch nicht gibt (die Datei ist älter
    /// als das Update), kommen aus den eingebauten Werten dazu.
    pub fn load(path: &std::path::Path) -> Result<Economy, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Economy::parse_with_builtin(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Economy::parse(ECONOMY_JSON),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Wie `parse`, aber was in `json` fehlt (neue Gebäude samt Rezepten, neue Einheiten),
    /// kommt aus der eingebauten economy.json. Werte in `json` bleiben, wie sie sind.
    pub fn parse_with_builtin(json: &str) -> Result<Economy, String> {
        let mut file = Economy::parse(json)?;
        let builtin = Economy::parse(ECONOMY_JSON).expect("eingebaute economy.json ist gültig");
        let added = file.def.add_missing_from(&builtin.def);
        if added.is_empty() {
            return Ok(file);
        }
        tracing::info!("Wirtschaftsdaten: ergänzt aus den eingebauten Werten: {}", added.join(", "));
        let json = serde_json::to_string(&file.def).map_err(|e| e.to_string())?;
        Economy::parse(&json)
    }

    /// Als JSON im Stil der Datei: kurze Teile auf einer Zeile, sonst eingerückt.
    pub fn to_json(&self) -> String {
        let mut def = self.def.clone();
        def.format = FORMAT;
        let mut value = serde_json::to_value(&def).expect("serialisierbar");
        tidy(&mut value);
        let mut out = String::new();
        write_json(&mut out, &value, 0, 0, 0);
        out.push('\n');
        out
    }

    /// Wie viel ein Feld mit diesem Rohstoff anfangs hergibt (0 = unbegrenzt bzw. nichts zum Abbauen).
    pub fn resource_amount(&self, r: ResourceKind) -> u32 {
        self.def.resource_amounts.iter().find(|a| a.resource == r).map_or(0, |a| a.amount)
    }

    pub fn building(&self, kind: BuildingKind) -> Option<&BuildingDef> {
        self.buildings.get(&kind).map(|&i| &self.def.buildings[i])
    }

    pub fn unit(&self, kind: UnitKind) -> Option<&UnitDef> {
        self.def.units.iter().find(|u| u.kind == kind)
    }

    /// Schadenswerte einer Einheit nach Art. Ohne Liste zählt `attack` (Nah- oder Fernkampf
    /// je nach Reichweite).
    pub fn damage_of(&self, def: &UnitDef) -> Vec<Damage> {
        if !def.damage.is_empty() {
            return def.damage.clone();
        }
        if def.attack == 0 {
            return Vec::new();
        }
        vec![Damage { kind: default_damage_type(def.kind, self.is_ranged(def.range)), value: def.attack }]
    }

    /// Schaden `x` der Art `kind` gegen diese Rüstung: Y = (X − min) × (1 − block), gerundet.
    /// Wehrt die Rüstung nicht alles ab, bleibt mindestens `army.min_damage`.
    pub fn damage_against(&self, x: u32, kind: DamageType, armor: &[protocol::Armor]) -> u32 {
        if x == 0 {
            return 0;
        }
        let Some(a) = armor.iter().find(|a| a.kind == kind) else { return x };
        let block = a.block.clamp(0.0, 1.0);
        if block >= 1.0 {
            return 0;
        }
        let y = (x.saturating_sub(a.min) as f32 * (1.0 - block)).round() as u32;
        y.max(self.def.army.min_damage.min(x))
    }

    /// Schaden eines Treffers dieser Einheit gegen diese Rüstung: die Art, die am meisten anrichtet.
    pub fn hit_damage(&self, attacker: &UnitDef, armor: &[protocol::Armor]) -> u32 {
        self.damage_of(attacker).iter().map(|d| self.damage_against(d.value, d.kind, armor)).max().unwrap_or(0)
    }

    /// Wie `hit_damage`, aber gegen Einheiten: Belagerungsschaden gilt bei Belagerungsgeräten nur
    /// gegen Gebäude (ihr Stein trifft Einheiten mit den übrigen Schadensarten).
    pub fn unit_hit_damage(&self, attacker: &UnitDef, armor: &[protocol::Armor]) -> u32 {
        if attacker.siege.is_none() {
            return self.hit_damage(attacker, armor);
        }
        self.damage_of(attacker)
            .iter()
            .filter(|d| d.kind != DamageType::Siege)
            .map(|d| self.damage_against(d.value, d.kind, armor))
            .max()
            .unwrap_or(0)
    }

    /// Zählt ein Angriff mit dieser Reichweite als Fernkampf?
    pub fn is_ranged(&self, range: f32) -> bool {
        range > self.def.army.melee_range
    }

    pub fn recipe(&self, id: &str) -> Option<&RecipeDef> {
        self.def.recipes.iter().find(|r| r.id == id)
    }
}

/// Grund-Schadensart einer Einheit, wenn die Datei nur `attack` kennt.
fn default_damage_type(kind: UnitKind, ranged: bool) -> DamageType {
    match (kind, ranged) {
        (UnitKind::Crossbowman, _) => DamageType::HeavyRanged,
        (_, true) => DamageType::Ranged,
        (UnitKind::Swordsman | UnitKind::Knight, _) => DamageType::HeavyMelee,
        _ => DamageType::Melee,
    }
}

/// Schadensliste und `attack` passend machen: ohne Liste wird `attack` zur Liste (eine Art,
/// je nach Einheit), mit Liste ist `attack` der größte Wert darin.
fn fill_damage(def: &mut EconomyDef) {
    let melee_range = def.army.melee_range;
    for u in &mut def.units {
        u.damage.retain(|d| d.value > 0);
        if u.damage.is_empty() && u.attack > 0 && u.range > 0.0 {
            u.damage.push(Damage { kind: default_damage_type(u.kind, u.range > melee_range), value: u.attack });
        }
        if !u.damage.is_empty() {
            u.attack = u.damage.iter().map(|d| d.value).max().unwrap_or(0);
        }
    }
}

/// Datei vor Format 3 kennt noch keine Rüstung: die eingebauten Rüstungen übernehmen (Einheiten,
/// Pferde, Gebäude je Stufe) und Schadensarten, die die Einheit noch nicht hat (Ritter: Belagerung).
/// Die eigenen Werte der Datei (Lebenspunkte, Schaden …) bleiben.
fn add_armor_from_builtin(def: &mut EconomyDef) {
    let Some(base) = builtin_def() else { return };
    let mut base = base;
    fill_damage(&mut base);
    let melee_range = def.army.melee_range;
    for u in &mut def.units {
        let Some(b) = base.units.iter().find(|b| b.kind == u.kind) else { continue };
        if u.armor.is_empty() {
            u.armor = b.armor.clone();
        }
        if u.horse_armor.is_empty() {
            u.horse_armor = b.horse_armor.clone();
        }
        if u.damage.is_empty() && u.attack > 0 && u.range > 0.0 {
            let own = default_damage_type(u.kind, u.range > melee_range);
            u.damage.push(Damage { kind: own, value: u.attack });
            u.damage.extend(b.damage.iter().filter(|d| d.kind != own).copied());
        }
    }
    for d in &mut def.buildings {
        let Some(b) = base.buildings.iter().find(|b| b.kind == d.kind) else { continue };
        for (i, l) in d.levels.iter_mut().enumerate() {
            if l.armor.is_empty() {
                l.armor = b.at(i as u32 + 1).armor.clone();
            }
        }
    }
}

/// Ältere Balancing-Dateien kennen Apotheke, Heilsalbe und Heiler noch nicht: aus den
/// eingebauten Werten übernehmen, was fehlt (Salbe kommt dann auch in die Waffenkammer).
fn add_apothecary(def: &mut EconomyDef) {
    use protocol::Good;
    let missing_good = !def.goods.contains(&Good::Salve);
    let missing_building = !def.buildings.iter().any(|b| b.kind == BuildingKind::Apothecary);
    let missing_unit = !def.units.iter().any(|u| u.kind == UnitKind::Healer);
    if !(missing_good || missing_building || missing_unit) {
        return;
    }
    let Some(base) = builtin_def() else { return };
    if missing_good {
        def.goods.push(Good::Salve);
        for b in def.buildings.iter_mut().filter(|b| b.kind == BuildingKind::Armory && !b.hub_goods.is_empty()) {
            b.hub_goods.push(Good::Salve);
        }
    }
    if missing_building {
        if let Some(b) = base.buildings.iter().find(|b| b.kind == BuildingKind::Apothecary) {
            def.buildings.push(b.clone());
            for r in base.recipes.iter().filter(|r| r.building == BuildingKind::Apothecary) {
                if !def.recipes.iter().any(|x| x.id == r.id) {
                    def.recipes.push(r.clone());
                }
            }
        }
    }
    if missing_unit {
        if let Some(u) = base.units.iter().find(|u| u.kind == UnitKind::Healer) {
            def.units.push(u.clone());
        }
    }
}

/// Zeilen höchstens so lang, sonst wird umgebrochen.
const JSON_WIDTH: usize = 100;

fn write_json(out: &mut String, v: &serde_json::Value, indent: usize, depth: usize, prefix: usize) {
    use serde_json::Value;
    let compact = compact_json(v);
    if depth >= 2 && indent + prefix + compact.len() < JSON_WIDTH {
        out.push_str(&compact);
        return;
    }
    let pad = " ".repeat(indent + 2);
    match v {
        Value::Object(m) if !m.is_empty() => {
            out.push_str("{\n");
            for (i, (k, x)) in m.iter().enumerate() {
                let key = serde_json::to_string(k).expect("Schlüssel");
                out.push_str(&format!("{pad}{key}: "));
                write_json(out, x, indent + 2, depth + 1, key.len() + 2);
                out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
        Value::Array(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad);
                write_json(out, x, indent + 2, depth + 1, 0);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&" ".repeat(indent));
            out.push(']');
        }
        _ => out.push_str(&compact),
    }
}

/// Für eine gut lesbare Datei: leere Standardwerte weglassen und Kommazahlen
/// so kurz schreiben, wie sie als f32 gemeint sind (1.2 statt 1.2000000476837158).
fn tidy(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            m.retain(|k, x| LEVEL_FIELDS.contains(&k.as_str()) || match x {
                Value::Null => false,
                Value::Array(a) if a.is_empty() => k != "gathers",
                Value::Bool(false) => k != "hub",
                Value::Number(n) if n.as_u64() == Some(0) => k != "gather_radius",
                _ => true,
            });
            m.values_mut().for_each(tidy);
        }
        Value::Array(a) => a.iter_mut().for_each(tidy),
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or_default() as f32;
            *v = if f.fract() == 0.0 && f.abs() < 1e9 {
                Value::from(f as i64)
            } else {
                serde_json::from_str(&format!("{f}")).unwrap_or(Value::Null)
            };
        }
        _ => {}
    }
}

/// Eine Zeile mit `, ` und `: ` wie in der Datei.
fn compact_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, x)| format!("{}: {}", serde_json::to_string(k).expect("Schlüssel"), compact_json(x)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(compact_json).collect::<Vec<_>>().join(", ")),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alte_dateien_bekommen_die_apotheke() {
        let mut v: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        v["goods"].as_array_mut().unwrap().retain(|g| g != "salve");
        v["buildings"].as_array_mut().unwrap().retain(|b| b["kind"] != "apothecary");
        for b in v["buildings"].as_array_mut().unwrap() {
            b["hub_goods"].as_array_mut().unwrap().retain(|g| g != "salve");
        }
        v["recipes"].as_array_mut().unwrap().retain(|r| r["building"] != "apothecary");
        v["units"].as_array_mut().unwrap().retain(|u| u["kind"] != "healer");
        v["market"]["prices"].as_array_mut().unwrap().retain(|p| p["good"] != "salve");
        let eco = Economy::parse(&v.to_string()).unwrap();
        assert!(eco.building(BuildingKind::Apothecary).is_some());
        assert!(eco.unit(UnitKind::Healer).is_some_and(|u| u.heal > 0));
        assert!(eco.recipe("make_salve").is_some());
        assert!(eco.building(BuildingKind::Armory).unwrap().hub_goods.contains(&protocol::Good::Salve));
    }

    #[test]
    fn wirtschaftsdaten_sind_gueltig() {
        let eco = Economy::parse(ECONOMY_JSON).unwrap();
        // Jede Gebäudeart aus dem Protokoll hat Daten (serde kennt alle Varianten).
        for b in &eco.def.buildings {
            assert!(eco.building(b.kind).is_some());
        }
        assert_eq!(eco.def.buildings.len(), 39);
        assert!(eco.def.buildings.iter().any(|b| b.hub), "ohne Lager kein Transport");
    }

    #[test]
    fn saegewerk_und_einfache_betriebe_brauchen_nur_holz() {
        // Jans Regel: aus Holz allein lässt sich alles wieder aufbauen.
        let eco = Economy::parse(ECONOMY_JSON).unwrap();
        use BuildingKind::*;
        for kind in [Woodcutter, Sawmill, Stonemason, PigFarm, SpearMaker] {
            let cost = &eco.building(kind).unwrap().levels[0].cost;
            assert!(cost.iter().all(|g| g.good == protocol::Good::Wood), "{kind:?}");
        }
        for kind in [Bakery, Smelter, Smithy] {
            let cost = &eco.building(kind).unwrap().levels[0].cost;
            assert!(cost.iter().any(|g| g.good == protocol::Good::Stone), "{kind:?} braucht Stein");
            assert!(cost.iter().any(|g| g.good == protocol::Good::Boards), "{kind:?} braucht Bretter");
        }
        for kind in [Bowyer, Mill, CattleFarm] {
            let cost = &eco.building(kind).unwrap().levels[0].cost;
            assert!(cost.iter().any(|g| g.good == protocol::Good::Boards), "{kind:?} braucht Bretter");
        }
    }

    #[test]
    fn speichern_und_wieder_laden_ergibt_dasselbe() {
        let eco = Economy::parse(ECONOMY_JSON).unwrap();
        let text = eco.to_json();
        let again = Economy::parse(&text).unwrap();
        assert_eq!(again.to_json(), text);
        assert!(text.lines().all(|l| l.len() <= 120), "kurze Zeilen");
    }

    /// Balancing-Datei im alten Aufbau (Werte einmal je Gebäude), Stand vor Format 2.
    const FORMAT1: &str = include_str!("../data/tests/economy_format1.json");

    #[test]
    fn alte_dateien_werden_umgewandelt() {
        let old: serde_json::Value = serde_json::from_str(FORMAT1).unwrap();
        let eco = Economy::parse(FORMAT1).unwrap();
        assert_eq!(eco.def.format, FORMAT);
        for (b, ob) in eco.def.buildings.iter().zip(old["buildings"].as_array().unwrap()) {
            let walk = ob["walkway_from_level"].as_u64().unwrap_or(0) as usize;
            let stairs = ob["stairs_from_level"].as_u64().unwrap_or(0) as usize;
            for (i, l) in b.levels.iter().enumerate() {
                let n = i + 1;
                assert_eq!(l.max_hp as u64, ob["max_hp"].as_u64().unwrap(), "{:?}", b.kind);
                assert_eq!(l.garrison as u64, ob["garrison"].as_u64().unwrap_or(0), "{:?}", b.kind);
                assert_eq!(l.workers as u64, ob["workers"].as_u64().unwrap_or(0), "{:?}", b.kind);
                assert_eq!(l.walkway, walk > 0 && n >= walk, "{:?} Stufe {n}", b.kind);
                assert_eq!(l.stairs, stairs > 0 && n >= stairs, "{:?} Stufe {n}", b.kind);
                assert_eq!(l.cover_percent, 0, "alte Dateien kennen keine Deckung");
            }
            assert_eq!(b.build_radius as u64, ob["build_radius"].as_u64().unwrap_or(0), "{:?}", b.kind);
            assert_eq!(b.delivery, protocol::DeliveryDefaults::default(), "Voreinstellung: Träger, Karren und Arbeiter");
        }
        let old_attack = old["army"]["attack_seconds"].as_f64().unwrap() as f32;
        // Einheiten, die die alte Datei schon hatte (neue wie die Belagerungsgeräte kommen aus den eingebauten Werten).
        let old_units: Vec<&str> = old["units"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|u| u["kind"].as_str()).collect());
        let had = |u: &&protocol::UnitDef| old_units.contains(&serde_json::to_value(u.kind).unwrap().as_str().unwrap_or(""));
        assert!(eco.def.units.iter().filter(had).all(|u| u.attack_seconds == old_attack), "jede Einheit bekommt den alten Takt");
        // Umgewandelt gespeichert und wieder geladen: dasselbe.
        let text = eco.to_json();
        assert_eq!(Economy::parse(&text).unwrap().to_json(), text);
    }

    #[test]
    fn werte_je_stufe_bleiben_beim_speichern_erhalten() {
        let mut eco = Economy::parse(ECONOMY_JSON).unwrap();
        let tower = eco.def.buildings.iter_mut().find(|b| b.kind == BuildingKind::Tower).unwrap();
        // Der Test braucht zwei Stufen; hat das Balancing nur eine, kommt hier eine dazu.
        if tower.levels.len() < 2 {
            tower.levels.push(tower.levels[0].clone());
        }
        tower.levels[0].garrison = 8;
        tower.levels[1].garrison = 0;
        tower.levels[0].walkway = true;
        tower.levels[1].walkway = false;
        tower.levels[1].cover_percent = 60;
        let again = Economy::parse(&eco.to_json()).unwrap();
        let t = again.building(BuildingKind::Tower).unwrap();
        assert_eq!((t.at(1).garrison, t.at(2).garrison), (8, 0), "0 auf Stufe 2 erbt nicht von Stufe 1");
        assert_eq!((t.at(1).walkway, t.at(2).walkway), (true, false));
        assert_eq!(t.at(2).cover_percent, 60);
        assert_eq!(t.at(99).garrison, 0, "über der letzten Stufe gilt die letzte");
    }

    #[test]
    fn fehlende_werte_erbt_eine_stufe_von_der_davor() {
        let mut v: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        let tower = v["buildings"].as_array_mut().unwrap().iter_mut().find(|b| b["kind"] == "tower").unwrap();
        let levels = tower["levels"].as_array_mut().unwrap();
        if levels.len() < 2 {
            let first = levels[0].clone();
            levels.push(first);
        }
        tower["levels"][0]["garrison"] = 7.into();
        tower["levels"][1].as_object_mut().unwrap().remove("garrison");
        let eco = Economy::parse(&v.to_string()).unwrap();
        assert_eq!(eco.building(BuildingKind::Tower).unwrap().at(2).garrison, 7);
    }

    #[test]
    fn alte_datei_bekommt_neue_gebaeude_dazu() {
        // Eine Balancing-Datei von vor der Pferdezucht: Zucht, Rezept und Ware kommen dazu.
        let mut v: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        v["buildings"].as_array_mut().unwrap().retain(|b| b["kind"] != "horse_farm");
        v["recipes"].as_array_mut().unwrap().retain(|r| r["building"] != "horse_farm");
        v["goods"].as_array_mut().unwrap().retain(|g| g != "horse");
        let eco = Economy::parse(&v.to_string()).unwrap();
        assert!(eco.building(BuildingKind::HorseFarm).is_some());
        assert!(eco.def.goods.contains(&protocol::Good::Horse));
    }

    #[test]
    fn bauradius_je_stufe_wird_einer_je_gebaeude() {
        // Format 3: `build_radius` stand in jeder Stufe. Der größte Wert gilt jetzt für alle Stufen.
        let mut v: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        v["format"] = serde_json::Value::from(3);
        for b in v["buildings"].as_array_mut().unwrap() {
            let b = b.as_object_mut().unwrap();
            b.remove("build_radius");
            for (i, l) in b.get_mut("levels").unwrap().as_array_mut().unwrap().iter_mut().enumerate() {
                l["build_radius"] = serde_json::Value::from(if i == 0 { 20 } else { 35 });
            }
        }
        let eco = Economy::parse(&v.to_string()).unwrap();
        let wall = eco.building(BuildingKind::Wall).unwrap();
        assert_eq!(wall.build_radius, if wall.levels.len() > 1 { 35 } else { 20 });
        assert!(!eco.to_json().contains("\"build_radius\": 35,\n          \"road"), "nicht mehr je Stufe");
    }

    #[test]
    fn alte_datei_bekommt_ruestung_und_schadensarten() {
        let mut v: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        v["format"] = serde_json::Value::from(2);
        for u in v["units"].as_array_mut().unwrap() {
            let u = u.as_object_mut().unwrap();
            for k in ["damage", "armor", "horse_armor"] {
                u.remove(k);
            }
            let king = u["kind"] == "king";
            u.insert("ranged_immune".into(), serde_json::Value::Bool(king));
            if u["kind"] == "knight" {
                u.insert("attack".into(), serde_json::Value::from(33));
            }
        }
        for b in v["buildings"].as_array_mut().unwrap() {
            for l in b["levels"].as_array_mut().unwrap() {
                l.as_object_mut().unwrap().remove("armor");
            }
        }
        let eco = Economy::parse(&v.to_string()).unwrap();
        let builtin = Economy::parse(ECONOMY_JSON).unwrap();
        let knight = eco.unit(UnitKind::Knight).unwrap();
        assert_eq!(knight.damage[0], Damage { kind: DamageType::HeavyMelee, value: 33 }, "eigener Angriff bleibt");
        assert!(knight.damage.iter().any(|d| d.kind == DamageType::Siege), "Belagerung aus den eingebauten Werten");
        assert_eq!(knight.armor, builtin.unit(UnitKind::Knight).unwrap().armor);
        assert_eq!(knight.horse_armor, builtin.unit(UnitKind::Knight).unwrap().horse_armor);
        let king = eco.unit(UnitKind::King).unwrap();
        assert_eq!(eco.hit_damage(eco.unit(UnitKind::Archer).unwrap(), &king.armor), 0, "König immun gegen Fernkampf");
        let wall = eco.building(BuildingKind::Wall).unwrap();
        assert_eq!(wall.at(2).armor, builtin.building(BuildingKind::Wall).unwrap().at(2).armor);
        // Ausdrücklich nicht immun: keine eingebaute Immunität.
        for u in v["units"].as_array_mut().unwrap() {
            if u["kind"] == "king" {
                u["ranged_immune"] = serde_json::Value::Bool(false);
            }
        }
        let eco = Economy::parse(&v.to_string()).unwrap();
        let king = eco.unit(UnitKind::King).unwrap();
        assert!(eco.hit_damage(eco.unit(UnitKind::Archer).unwrap(), &king.armor) > 0);
        // Und nach dem Speichern (Format 3) bleibt es so.
        let again = Economy::parse(&eco.to_json()).unwrap();
        assert!(again.hit_damage(again.unit(UnitKind::Archer).unwrap(), &again.unit(UnitKind::King).unwrap().armor) > 0);
    }

    #[test]
    fn schaden_gegen_ruestung() {
        let eco = Economy::parse(ECONOMY_JSON).unwrap();
        let a = |min, block| [protocol::Armor { kind: DamageType::Melee, min, block }];
        assert_eq!(eco.damage_against(20, DamageType::Melee, &a(4, 0.5)), 8);
        assert_eq!(eco.damage_against(20, DamageType::Ranged, &a(4, 0.5)), 20);
        assert_eq!(eco.damage_against(3, DamageType::Melee, &a(5, 0.0)), eco.def.army.min_damage);
        assert_eq!(eco.damage_against(30, DamageType::Melee, &a(0, 1.0)), 0);
    }

    #[test]
    fn fehler_werden_erkannt() {
        let bad = ECONOMY_JSON.replacen("\"make_crossbow\"]", "\"gibt_es_nicht\"]", 1);
        assert!(Economy::parse(&bad).is_err());
    }
}

/// Ergänzt fehlende Teile aus einer anderen Wirtschaft (der eingebauten).
trait AddMissing {
    /// Gibt zurück, was dazukam (für das Protokoll).
    fn add_missing_from(&mut self, builtin: &EconomyDef) -> Vec<String>;
}

impl AddMissing for EconomyDef {
    fn add_missing_from(&mut self, builtin: &EconomyDef) -> Vec<String> {
        let mut added = Vec::new();
        // Gebäude in der Reihenfolge der eingebauten Liste (so bleibt das Baumenü geordnet).
        let mut buildings = Vec::new();
        let mut new_kinds = Vec::new();
        for b in &builtin.buildings {
            match self.buildings.iter().position(|o| o.kind == b.kind) {
                Some(i) => buildings.push(self.buildings[i].clone()),
                None => {
                    buildings.push(b.clone());
                    new_kinds.push(b.kind);
                    added.push(format!("{:?}", b.kind));
                }
            }
        }
        // Gebäude, die nur in der Datei stehen, bleiben hinten dran.
        for b in &self.buildings {
            if !builtin.buildings.iter().any(|o| o.kind == b.kind) {
                buildings.push(b.clone());
            }
        }
        self.buildings = buildings;
        for r in builtin.recipes.iter().filter(|r| new_kinds.contains(&r.building)) {
            if !self.recipes.iter().any(|o| o.id == r.id) {
                self.recipes.push(r.clone());
            }
        }
        // Ältere Dateien kennen das Startgebäude noch nicht (oder der Balance-Editor hat das
        // fehlende Feld als null zurückgeschrieben): dann gilt das eingebaute (der Bergfried).
        if self.start_building.is_none() && builtin.start_building.is_some() {
            self.start_building = builtin.start_building;
            added.push("start_building".into());
        }
        for u in &builtin.units {
            if !self.units.iter().any(|o| o.kind == u.kind) {
                self.units.push(u.clone());
                added.push(format!("{:?}", u.kind));
            }
        }
        // Waren, die die neuen Teile brauchen.
        let used: Vec<Good> = self
            .buildings
            .iter()
            .flat_map(|b| b.levels.iter().flat_map(|l| l.cost.iter().map(|g| g.good)))
            .chain(self.recipes.iter().flat_map(|r| r.inputs.iter().chain(&r.outputs).map(|g| g.good)))
            .chain(self.units.iter().flat_map(|u| u.cost.iter().map(|g| g.good)))
            .collect();
        for g in builtin.goods.iter().filter(|g| used.contains(g)) {
            if !self.goods.contains(g) {
                self.goods.push(*g);
            }
        }
        added
    }
}

#[cfg(test)]
mod builtin_tests {
    use super::*;

    #[test]
    fn alte_datei_bekommt_neue_gebaeude() {
        let full = Economy::parse(ECONOMY_JSON).unwrap();
        let mut old = full.def.clone();
        old.buildings.retain(|b| !matches!(b.kind, BuildingKind::WoodPath | BuildingKind::StonePath));
        old.buildings[0].levels[0].cost.clear();
        let json = serde_json::to_string(&old).unwrap();
        let merged = Economy::parse_with_builtin(&json).unwrap();
        assert!(merged.building(BuildingKind::WoodPath).is_some(), "Holzweg ergänzt");
        assert!(merged.building(BuildingKind::StonePath).is_some(), "Steinweg ergänzt");
        let kinds: Vec<_> = merged.def.buildings.iter().map(|b| b.kind).collect();
        let builtin: Vec<_> = full.def.buildings.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, builtin, "Reihenfolge wie eingebaut");
        let first = merged.def.buildings.iter().find(|b| b.kind == old.buildings[0].kind).unwrap();
        assert!(first.levels[0].cost.is_empty(), "eigene Werte bleiben");
    }

    #[test]
    fn alte_datei_ohne_startgebaeude_bekommt_bergfried() {
        let mut value: serde_json::Value = serde_json::from_str(ECONOMY_JSON).unwrap();
        value.as_object_mut().unwrap().remove("start_building");
        let merged = Economy::parse_with_builtin(&value.to_string()).unwrap();
        assert_eq!(merged.def.start_building, Some(BuildingKind::Keep));
        value.as_object_mut().unwrap().insert("start_building".into(), serde_json::Value::Null);
        let merged = Economy::parse_with_builtin(&value.to_string()).unwrap();
        assert_eq!(merged.def.start_building, Some(BuildingKind::Keep));
    }
}
