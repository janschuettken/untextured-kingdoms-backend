//! Wirtschaft als Daten: Waren, Gebäude mit Stufen und Kosten, Rezepte.
//!
//! Alles steht in `server/data/economy.json` (beim Kompilieren eingebettet).
//! Die Datei geht unverändert im `welcome` an den Client, damit Baumenü,
//! Kosten und später die Übersicht der Produktionsketten daraus entstehen.
//! Balancing heißt also: nur diese Datei ändern.

use std::collections::HashMap;

use protocol::{BuildingDef, BuildingKind, EconomyDef, Good, GoodAmount, RecipeDef, ResourceKind, UnitDef, UnitKind};

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
        range: 1.2,
        upkeep: 0,
        food: 0,
        ranged_immune: true,
        attack_seconds: 1.5,
    }
}

/// Aktueller Aufbau der Balancing-Datei.
pub const FORMAT: u32 = 2;

/// Werte, die je Gebäudestufe gelten. Fehlen sie in einer Stufe, gilt die Stufe davor.
const LEVEL_FIELDS: [&str; 14] = [
    "max_hp", "workers", "gather_radius", "service_radius", "walkway", "stairs", "gate_radius", "garrison",
    "cover_percent", "build_radius", "road_speed_percent", "hub_capacity", "storage_inputs", "storage_outputs",
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
        K::WoodPath | K::StonePath => 0.0,
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
            b.levels.iter_mut().for_each(|l| l.build_radius = 0);
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
                def.units.push(UnitDef { kind, hp, ranged_immune: false, attack: 0, range: 0.0, ..default_king(1.0) });
            }
        }
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
            if !u.kind.is_soldier() && u.kind != UnitKind::King && !u.kind.is_civilian() {
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
            if !(u.seconds > 0.0 && u.seconds.is_finite() && u.speed > 0.0 && u.speed.is_finite()) {
                return Err(format!("Einheit {:?}: Ausbildungszeit und Tempo müssen größer als 0 sein", u.kind));
            }
            if u.hp == 0 {
                return Err(format!("Einheit {:?}: braucht Lebenspunkte", u.kind));
            }
            if !(u.attack_seconds > 0.0 && u.attack_seconds.is_finite()) {
                return Err(format!("Einheit {:?}: Sekunden zwischen zwei Angriffen müssen größer als 0 sein", u.kind));
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
        if !(army.melee_range >= 0.0 && army.melee_range.is_finite()) {
            return Err("Heer: melee_range darf nicht negativ sein".into());
        }
        if !(army.enemy_block_radius >= 0.0 && army.enemy_block_radius.is_finite()) {
            return Err("Heer: enemy_block_radius darf nicht negativ sein (0 = aus)".into());
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

    /// Zählt ein Angriff mit dieser Reichweite als Fernkampf?
    pub fn is_ranged(&self, range: f32) -> bool {
        range > self.def.army.melee_range
    }

    pub fn recipe(&self, id: &str) -> Option<&RecipeDef> {
        self.def.recipes.iter().find(|r| r.id == id)
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
    fn wirtschaftsdaten_sind_gueltig() {
        let eco = Economy::parse(ECONOMY_JSON).unwrap();
        // Jede Gebäudeart aus dem Protokoll hat Daten (serde kennt alle Varianten).
        for b in &eco.def.buildings {
            assert!(eco.building(b.kind).is_some());
        }
        assert_eq!(eco.def.buildings.len(), 34);
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
                assert_eq!(l.build_radius as u64, ob["build_radius"].as_u64().unwrap_or(0), "{:?}", b.kind);
                assert_eq!(l.walkway, walk > 0 && n >= walk, "{:?} Stufe {n}", b.kind);
                assert_eq!(l.stairs, stairs > 0 && n >= stairs, "{:?} Stufe {n}", b.kind);
                assert_eq!(l.cover_percent, 0, "alte Dateien kennen keine Deckung");
            }
            assert_eq!(b.delivery, protocol::DeliveryDefaults::default(), "wie bisher: Träger und Arbeiter, keine Karren");
        }
        let old_attack = old["army"]["attack_seconds"].as_f64().unwrap() as f32;
        assert!(eco.def.units.iter().all(|u| u.attack_seconds == old_attack), "jede Einheit bekommt den alten Takt");
        // Umgewandelt gespeichert und wieder geladen: dasselbe.
        let text = eco.to_json();
        assert_eq!(Economy::parse(&text).unwrap().to_json(), text);
    }

    #[test]
    fn werte_je_stufe_bleiben_beim_speichern_erhalten() {
        let mut eco = Economy::parse(ECONOMY_JSON).unwrap();
        let tower = eco.def.buildings.iter_mut().find(|b| b.kind == BuildingKind::Tower).unwrap();
        assert!(tower.levels.len() >= 2);
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
        tower["levels"][0]["garrison"] = 7.into();
        tower["levels"][1].as_object_mut().unwrap().remove("garrison");
        let eco = Economy::parse(&v.to_string()).unwrap();
        assert_eq!(eco.building(BuildingKind::Tower).unwrap().at(2).garrison, 7);
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
}
