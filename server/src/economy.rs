//! Wirtschaft als Daten: Waren, Gebäude mit Stufen und Kosten, Rezepte.
//!
//! Alles steht in `server/data/economy.json` (beim Kompilieren eingebettet).
//! Die Datei geht unverändert im `welcome` an den Client, damit Baumenü,
//! Kosten und später die Übersicht der Produktionsketten daraus entstehen.
//! Balancing heißt also: nur diese Datei ändern.

use std::collections::HashMap;

use protocol::{BuildingDef, BuildingKind, EconomyDef, GoodAmount, RecipeDef, ResourceKind, UnitDef, UnitKind};

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
            b.workers = 0;
        }
        self.def.population.per_transport = 0;
        self
    }

    /// Liest und prüft die Wirtschaftsdaten.
    pub fn parse(json: &str) -> Result<Economy, String> {
        let def: EconomyDef = serde_json::from_str(json).map_err(|e| format!("economy.json: {e}"))?;
        let mut buildings = HashMap::new();
        for (i, b) in def.buildings.iter().enumerate() {
            if buildings.insert(b.kind, i).is_some() {
                return Err(format!("Gebäude {:?} doppelt", b.kind));
            }
            if b.levels.is_empty() || b.w == 0 || b.h == 0 {
                return Err(format!("Gebäude {:?} braucht Größe und mindestens eine Stufe", b.kind));
            }
            if b.spawns.iter().chain(b.levels.iter().flat_map(|l| &l.spawns)).any(|s| !matches!(s.kind, UnitKind::Carrier | UnitKind::Cart)) {
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
            if !u.kind.is_soldier() {
                return Err(format!("Einheit {:?}: nur Soldaten werden in der Kaserne ausgebildet", u.kind));
            }
            if def.units[..i].iter().any(|v| v.kind == u.kind) {
                return Err(format!("Einheit {:?} doppelt", u.kind));
            }
            if !(u.seconds > 0.0 && u.seconds.is_finite() && u.speed > 0.0 && u.speed.is_finite()) {
                return Err(format!("Einheit {:?}: Ausbildungszeit und Tempo müssen größer als 0 sein", u.kind));
            }
            if u.hp == 0 {
                return Err(format!("Einheit {:?}: braucht Lebenspunkte", u.kind));
            }
        }
        for b in &def.buildings {
            for (n, level) in b.levels.iter().enumerate() {
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
    pub fn load(path: &std::path::Path) -> Result<Economy, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Economy::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Economy::parse(ECONOMY_JSON),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Als JSON im Stil der Datei: kurze Teile auf einer Zeile, sonst eingerückt.
    pub fn to_json(&self) -> String {
        let mut value = serde_json::to_value(&self.def).expect("serialisierbar");
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
            m.retain(|k, x| match x {
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
        assert_eq!(eco.def.buildings.len(), 29);
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

    #[test]
    fn fehler_werden_erkannt() {
        let bad = ECONOMY_JSON.replacen("\"make_crossbow\"]", "\"gibt_es_nicht\"]", 1);
        assert!(Economy::parse(&bad).is_err());
    }
}
