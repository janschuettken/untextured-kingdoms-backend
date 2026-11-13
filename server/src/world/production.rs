//! Produktion: Betriebe arbeiten ihr gewähltes Rezept ab.
//!
//! Ein Durchlauf nimmt beim Start die Eingangswaren aus dem Lager des
//! Spielers, läuft `seconds` lang und legt dann die Ergebnisse ins Lager.
//! Abbaubetriebe (Holzfäller, Steinbruch, Mine) brauchen zusätzlich ein
//! Rohstoff-Feld in Reichweite und verbrauchen es: Ist ein Waldfeld leer,
//! verschwindet der Wald und Einheiten können dort wieder laufen.
//!
//! Heute liegt alles in einem gemeinsamen Lager je Spieler. Mit dem Transport
//! kommen später Lager je Gebäude und Träger dazu; `accept_deliveries` in
//! den Einstellungen ist dafür schon da.

use protocol::{BuildingSettings, BuildingState, BuildingStatus, ResourceKind, ServerMsg, Text};

use super::{Refusal, World};
use crate::map::resource_blocks;

/// Laufender Durchlauf eines Gebäudes.
#[derive(Debug, Clone, Default)]
pub struct Work {
    /// Sekunden seit Start. `None` = noch nicht gestartet (Eingänge fehlen).
    elapsed: Option<f32>,
}

impl World {
    pub(super) fn step_production(&mut self, dt: f32) {
        for i in 0..self.buildings.len() {
            let status = self.step_building(i, dt);
            if self.buildings[i].status != status {
                self.buildings[i].status = status;
                self.events.push(ServerMsg::BuildingChanged { building: self.buildings[i].clone() });
            }
        }
    }

    fn step_building(&mut self, i: usize, dt: f32) -> BuildingStatus {
        let b = &self.buildings[i];
        let (id, owner, kind) = (b.id, b.owner, b.kind);
        if b.settings.paused {
            return BuildingStatus::Paused;
        }
        let Some(recipe) = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).cloned() else {
            return BuildingStatus::Idle;
        };
        let Some(def) = self.economy.building(kind).cloned() else { return BuildingStatus::Idle };
        let gathers = !def.gathers.is_empty();

        let elapsed = self.work.entry(id).or_default().elapsed;
        let elapsed = match elapsed {
            Some(e) => e,
            None => {
                // Starten: Rohstoff in Reichweite und Eingangswaren nötig.
                if gathers && self.gather_tile(i).is_none() {
                    return BuildingStatus::NoResource;
                }
                let Some(stock) = self.stocks.get_mut(&owner) else { return BuildingStatus::Idle };
                if !stock.take_all(&recipe.inputs) {
                    return BuildingStatus::MissingInput;
                }
                if !recipe.inputs.is_empty() {
                    self.dirty_stocks.insert(owner);
                }
                0.0
            }
        };
        let elapsed = elapsed + dt * self.speed(i);
        self.work.get_mut(&id).expect("eben angelegt").elapsed = Some(elapsed);
        if elapsed < recipe.seconds {
            return BuildingStatus::Working;
        }

        // Fertig: Ergebnis einlagern, wenn alles hineinpasst.
        let stock = self.stocks.get_mut(&owner).expect("Spieler hat ein Lager");
        if recipe.outputs.iter().any(|o| stock.amount(o.good) + o.amount > stock.capacity) {
            return BuildingStatus::StorageFull;
        }
        for o in &recipe.outputs {
            stock.add(o.good, o.amount);
        }
        self.dirty_stocks.insert(owner);
        if gathers {
            if let Some(tile) = self.gather_tile(i) {
                self.deplete(tile);
            }
        }
        self.work.insert(id, Work::default());
        BuildingStatus::Working
    }

    /// Höfe auf fruchtbarem Boden arbeiten schneller (anteilig nach Feldern).
    fn speed(&self, i: usize) -> f32 {
        let b = &self.buildings[i];
        let on_fields = self.economy.building(b.kind).and_then(|d| d.placement.as_ref()).is_some_and(|p| {
            p.resources.contains(&ResourceKind::Farmland) || p.resources.contains(&ResourceKind::Fertile)
        });
        if !on_fields {
            return 1.0;
        }
        let fertile = self.footprint(b).filter(|&t| self.map.resources[t] == ResourceKind::Fertile).count();
        1.0 + self.economy.def.fertile_bonus_percent as f32 / 100.0 * fertile as f32 / (b.w * b.h) as f32
    }

    fn footprint(&self, b: &BuildingState) -> impl Iterator<Item = usize> + '_ {
        let (x, y, w, h, mw) = (b.x, b.y, b.w, b.h, self.map.width);
        (y..y + h).flat_map(move |ty| (x..x + w).map(move |tx| (ty * mw + tx) as usize))
    }

    /// Nächstes Feld mit abbaubarem Rohstoff in Reichweite des Gebäudes.
    fn gather_tile(&self, i: usize) -> Option<usize> {
        let b = &self.buildings[i];
        let def = self.economy.building(b.kind)?;
        let r = def.gather_radius as i64;
        let (x0, y0) = (b.x as i64 - r, b.y as i64 - r);
        let (x1, y1) = (b.x as i64 + b.w as i64 - 1 + r, b.y as i64 + b.h as i64 - 1 + r);
        let (cx, cy) = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let mut best: Option<(f32, usize)> = None;
        for ty in y0.max(0)..=y1.min(self.map.height as i64 - 1) {
            for tx in x0.max(0)..=x1.min(self.map.width as i64 - 1) {
                let t = (ty as u32 * self.map.width + tx as u32) as usize;
                if !def.gathers.contains(&self.map.resources[t]) || self.resource_left[t] == 0 {
                    continue;
                }
                let d = (tx as f32 + 0.5 - cx).powi(2) + (ty as f32 + 0.5 - cy).powi(2);
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, t));
                }
            }
        }
        best.map(|(_, t)| t)
    }

    /// Ein Feld um eine Einheit abbauen. Leer: Rohstoff verschwindet, Feld wird frei.
    fn deplete(&mut self, t: usize) {
        self.resource_left[t] = self.resource_left[t].saturating_sub(1);
        if self.resource_left[t] == 0 {
            self.set_resource(t, ResourceKind::None);
        }
    }

    /// Rohstoff eines Feldes ändern, Laufgitter anpassen und alle Clients informieren.
    fn set_resource(&mut self, t: usize, r: ResourceKind) {
        let old = self.map.resources[t];
        if old == r {
            return;
        }
        let (x, y) = ((t as u32 % self.map.width) as i32, (t as u32 / self.map.width) as i32);
        if resource_blocks(old) {
            self.map.nav.set_blocked(x, y, false);
        }
        if resource_blocks(r) {
            self.map.nav.set_blocked(x, y, true);
        }
        self.map.resources[t] = r;
        self.resource_left[t] = self.economy.resource_amount(r);
        self.events.push(ServerMsg::ResourceChanged { x: x as u32, y: y as u32, resource: r });
        self.replan_all();
    }

    /// Einstellungen eines eigenen Gebäudes ändern. Rezepte nur, wenn freigeschaltet.
    pub fn configure(&mut self, player: u32, id: u32, settings: BuildingSettings) -> Result<BuildingState, Refusal> {
        let i = self.own_building(player, id)?;
        let b = &self.buildings[i];
        if let Some(r) = &settings.recipe {
            let def = self.economy.building(b.kind).ok_or_else(|| Text::new("error.unknown_building"))?;
            if !def.levels.iter().take(b.level as usize).any(|l| l.recipes.contains(r)) {
                return Err(Text::new("error.recipe_locked"));
            }
        }
        if b.settings.recipe != settings.recipe {
            // Rezept gewechselt: laufender Durchlauf verfällt (Eingänge sind verbraucht).
            self.work.remove(&id);
        }
        self.buildings[i].settings = settings;
        Ok(self.buildings[i].clone())
    }

    /// Obstplantage aufgeben: Gebäude verschwindet, auf der Fläche wächst Wald.
    pub fn release_trees(&mut self, player: u32, id: u32) -> Result<(), Refusal> {
        let i = self.own_building(player, id)?;
        let b = self.buildings[i].clone();
        let Some(r) = self.economy.building(b.kind).and_then(|d| d.releases_as) else {
            return Err(Text::new("error.cannot_release"));
        };
        self.remove_building(id);
        let tiles: Vec<usize> = self.footprint(&b).collect();
        for t in tiles {
            self.set_resource(t, r);
        }
        Ok(())
    }

    fn own_building(&self, player: u32, id: u32) -> Result<usize, Refusal> {
        match self.buildings.iter().position(|b| b.id == id) {
            Some(i) if self.buildings[i].owner == player => Ok(i),
            Some(_) => Err(Text::new("error.not_yours")),
            None => Err(Text::new("error.no_building")),
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount};

    use super::*;
    use crate::{economy::Economy, map::GameMap};

    fn world(text: &str) -> World {
        let mut w = World::new(GameMap::parse(text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap());
        w.spawn_player(1);
        w
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as usize {
            w.step(0.05);
        }
    }

    #[test]
    fn holzfaeller_holzt_wald_ab() {
        let mut w = world("spawn 0 0\n[terrain]\n000000\n000000\n000000\n[resources]\n.....W\n......\n......\n");
        let before = w.stocks[&1].amount(Good::Wood);
        let b = w.place_building(1, BuildingKind::Woodcutter, 1, 1).unwrap();
        let wood_after_build = w.stocks[&1].amount(Good::Wood);
        assert_eq!(wood_after_build, before - 20);
        assert!(!w.map.nav.is_open(5, 0), "Wald blockiert");
        // Ein Waldfeld gibt 20 Holz, ein Durchlauf dauert 6 s.
        run(&mut w, 20.0 * 6.0 + 1.0);
        assert_eq!(w.stocks[&1].amount(Good::Wood), wood_after_build + 20);
        assert_eq!(w.map.resources[5], ResourceKind::None);
        assert!(w.map.nav.is_open(5, 0), "abgeholzt: wieder begehbar");
        let events = w.take_events();
        assert!(events.iter().any(|e| matches!(e, ServerMsg::ResourceChanged { x: 5, y: 0, .. })));
        run(&mut w, 1.0);
        assert_eq!(w.buildings.iter().find(|x| x.id == b.id).unwrap().status, BuildingStatus::NoResource);
    }

    #[test]
    fn werkstatt_braucht_eingaenge_und_rezept_einstellbar() {
        let mut w = world("spawn 0 0\n000000\n000000\n000000\n");
        let b = w.place_building(1, BuildingKind::Bowyer, 2, 0).unwrap();
        assert_eq!(b.settings.recipe.as_deref(), Some("make_longbow"));
        // Armbrust erst ab Stufe 2.
        let mut s = b.settings.clone();
        s.recipe = Some("make_crossbow".into());
        assert_eq!(w.configure(1, b.id, s.clone()).unwrap_err().key, "error.recipe_locked");
        run(&mut w, 10.5);
        assert_eq!(w.stocks[&1].amount(Good::Longbow), 1);
        // Ohne Holz bleibt er stehen.
        let wood = w.stocks[&1].amount(Good::Wood);
        w.stocks.get_mut(&1).unwrap().take_all(&[GoodAmount { good: Good::Wood, amount: wood }]);
        run(&mut w, 11.0);
        assert_eq!(w.buildings[0].status, BuildingStatus::MissingInput);
        s.recipe = Some("make_longbow".into());
        s.paused = true;
        w.configure(1, b.id, s).unwrap();
        run(&mut w, 0.1);
        assert_eq!(w.buildings[0].status, BuildingStatus::Paused);
    }

    #[test]
    fn steinbruch_nur_auf_stein_bauernhof_auf_acker() {
        let mut w = world("spawn 5 2\n[terrain]\n000000\n000000\n000000\n[resources]\n...SS.\n...SS.\nAAF...\n");
        assert_eq!(w.place_building(1, BuildingKind::Quarry, 0, 0).unwrap_err().key, "error.needs_resource");
        w.place_building(1, BuildingKind::Quarry, 3, 0).unwrap();
        // Mine braucht Eisen, das es hier nicht gibt.
        assert_eq!(w.place_building(1, BuildingKind::Mine, 0, 0).unwrap_err().key, "error.needs_resource");
        // Bauernhof 3x3 auf 3 von 9 Ackerfeldern: zu wenig. Schweine gehen überall.
        assert_eq!(w.place_building(1, BuildingKind::Farm, 0, 0).unwrap_err().key, "error.needs_resource");
        w.place_building(1, BuildingKind::PigFarm, 0, 0).unwrap();
    }

    #[test]
    fn obstplantage_wird_zu_wald() {
        let mut w = world("spawn 5 5\n000000\n000000\n000000\n000000\n000000\n000000\n");
        let b = w.place_building(1, BuildingKind::Orchard, 0, 0).unwrap();
        run(&mut w, 26.0);
        assert_eq!(w.stocks[&1].amount(Good::Fruit), 2);
        assert_eq!(w.release_trees(1, b.id + 99).unwrap_err().key, "error.no_building");
        w.release_trees(1, b.id).unwrap();
        assert!(w.buildings.is_empty());
        assert_eq!(w.map.resources[0], ResourceKind::Forest);
        assert!(!w.map.nav.is_open(1, 1));
        // Darauf kann jetzt ein Holzfäller daneben arbeiten.
        w.place_building(1, BuildingKind::Woodcutter, 3, 0).unwrap();
        run(&mut w, 7.0);
        assert_eq!(w.buildings[0].status, BuildingStatus::Working);
    }
}
