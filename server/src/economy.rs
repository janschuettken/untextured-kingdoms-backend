//! Wirtschaft als Daten: Waren, Gebäude mit Stufen und Kosten, Rezepte.
//!
//! Alles steht in `server/data/economy.json` (beim Kompilieren eingebettet).
//! Die Datei geht unverändert im `welcome` an den Client, damit Baumenü,
//! Kosten und später die Übersicht der Produktionsketten daraus entstehen.
//! Balancing heißt also: nur diese Datei ändern.

use std::collections::HashMap;

use protocol::{BuildingDef, BuildingKind, EconomyDef, GoodAmount, RecipeDef, ResourceKind, UnitKind};

pub const ECONOMY_JSON: &str = include_str!("../data/economy.json");

pub struct Economy {
    pub def: EconomyDef,
    buildings: HashMap<BuildingKind, usize>,
}

impl Economy {
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
            if b.spawns.iter().any(|s| !matches!(s.kind, UnitKind::Carrier | UnitKind::Cart)) {
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
        let goods = |list: &[GoodAmount]| list.iter().all(|g| def.goods.contains(&g.good));
        if !def.buildings.iter().all(|b| b.levels.iter().all(|l| goods(&l.cost)))
            || !def.recipes.iter().all(|r| goods(&r.inputs) && goods(&r.outputs))
            || !goods(&def.start_stock)
        {
            return Err("Eine Ware wird benutzt, steht aber nicht in goods".into());
        }
        Ok(Economy { def, buildings })
    }

    /// Wie viel ein Feld mit diesem Rohstoff anfangs hergibt (0 = unbegrenzt bzw. nichts zum Abbauen).
    pub fn resource_amount(&self, r: ResourceKind) -> u32 {
        self.def.resource_amounts.iter().find(|a| a.resource == r).map_or(0, |a| a.amount)
    }

    pub fn building(&self, kind: BuildingKind) -> Option<&BuildingDef> {
        self.buildings.get(&kind).map(|&i| &self.def.buildings[i])
    }

    pub fn recipe(&self, id: &str) -> Option<&RecipeDef> {
        self.def.recipes.iter().find(|r| r.id == id)
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
        assert_eq!(eco.def.buildings.len(), 22);
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
    fn fehler_werden_erkannt() {
        let bad = ECONOMY_JSON.replacen("\"make_crossbow\"]", "\"gibt_es_nicht\"]", 1);
        assert!(Economy::parse(&bad).is_err());
    }
}
