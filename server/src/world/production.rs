//! Produktion: Betriebe arbeiten ihr gewähltes Rezept ab.
//!
//! Ein Durchlauf nimmt beim Start die Eingangswaren aus dem eigenen Lager des
//! Betriebs, läuft `seconds` lang und legt dann die Ergebnisse dort ab.
//! Hin und her tragen die Träger und Karren (siehe `transport.rs`).
//! Abbaubetriebe (Holzfäller, Steinbruch, Mine) brauchen zusätzlich ein
//! Rohstoff-Feld in Reichweite und verbrauchen es: Ist ein Waldfeld leer,
//! verschwindet der Wald und Einheiten können dort wieder laufen.

use protocol::{BuildingSettings, BuildingState, BuildingStatus, ResourceKind, ServerMsg, Text};

use super::{Refusal, World};
use crate::map::resource_blocks;

/// Laufender Durchlauf eines Gebäudes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
                self.dirty_buildings.insert(self.buildings[i].id);
            }
        }
    }

    fn step_building(&mut self, i: usize, dt: f32) -> BuildingStatus {
        let b = &self.buildings[i];
        let (id, kind) = (b.id, b.kind);
        if b.settings.paused {
            return BuildingStatus::Paused;
        }
        if self.unstaffed.contains(&id) {
            return BuildingStatus::NoWorkers;
        }
        let Some(recipe) = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)).cloned() else {
            return BuildingStatus::Idle;
        };
        let Some(def) = self.economy.building(kind).cloned() else { return BuildingStatus::Idle };
        let gathers = !def.gathers.is_empty();

        // Abbaubetriebe: die Arbeiter machen die Arbeit (siehe `gather.rs`).
        if gathers {
            return self.gather_status.get(&id).copied().unwrap_or(BuildingStatus::Working);
        }
        let elapsed = self.work.entry(id).or_default().elapsed;
        let elapsed = match elapsed {
            Some(e) => e,
            None => {
                // Starten: Rohstoff in Reichweite und Eingangswaren nötig.
                if gathers && self.gather_tile(i).is_none() {
                    return BuildingStatus::NoResource;
                }
                let Some(inv) = self.inventories.get_mut(&id) else { return BuildingStatus::Idle };
                if !inv.take_all(&recipe.inputs) {
                    return BuildingStatus::MissingInput;
                }
                if !recipe.inputs.is_empty() {
                    self.dirty_buildings.insert(id);
                }
                0.0
            }
        };
        let elapsed = elapsed + dt * self.speed(i) * self.hunger_factor(self.buildings[i].owner);
        self.work.get_mut(&id).expect("eben angelegt").elapsed = Some(elapsed);
        if elapsed < recipe.seconds {
            return BuildingStatus::Working;
        }

        // Fertig: Ergebnis ins eigene Lager, wenn alles hineinpasst. Abholen
        // ist Sache der Träger.
        let inv = self.inventories.get_mut(&id).expect("Betrieb hat ein Lager");
        if recipe.outputs.iter().any(|o| inv.amount(o.good) + o.amount > inv.capacity) {
            return BuildingStatus::StorageFull;
        }
        for o in &recipe.outputs {
            inv.add(o.good, o.amount);
        }
        self.dirty_buildings.insert(id);
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
    pub(super) fn gather_tile(&self, i: usize) -> Option<usize> {
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
    pub(super) fn deplete(&mut self, t: usize) {
        self.resource_left[t] = self.resource_left[t].saturating_sub(1);
        if self.resource_left[t] == 0 {
            self.set_resource(t, ResourceKind::None);
        }
    }

    /// Rohstoff eines Feldes ändern, Laufgitter anpassen und alle Clients informieren.
    pub(super) fn set_resource(&mut self, t: usize, r: ResourceKind) {
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
        let goods = &self.economy.def.goods;
        if settings.stores.iter().flatten().chain(settings.keep_in_stock.iter().map(|g| &g.good)).any(|g| !goods.contains(g)) {
            return Err(Text::new("error.unknown_good"));
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
        let mut w = World::new(GameMap::parse(text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_people());
        w.spawn_player(1);
        w
    }

    fn run(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as usize {
            w.step(0.05);
        }
    }

    /// Flache Karte `w` x `h` mit Rohstoff-Rechtecken (x, y, w, h, Zeichen).
    fn map(w: usize, h: usize, spawn: (u32, u32), rects: &[(usize, usize, usize, usize, char)]) -> String {
        let mut res = vec![vec!['.'; w]; h];
        for &(rx, ry, rw, rh, c) in rects {
            for row in res.iter_mut().skip(ry).take(rh) {
                for cell in row.iter_mut().skip(rx).take(rw) {
                    *cell = c;
                }
            }
        }
        let terrain = format!("{}\n", "0".repeat(w)).repeat(h);
        let resources: String = res.iter().map(|r| r.iter().collect::<String>() + "\n").collect();
        format!("spawn {} {}\n[terrain]\n{terrain}[resources]\n{resources}", spawn.0, spawn.1)
    }

    #[test]
    fn holzfaeller_holzt_wald_ab() {
        let mut w = world(&map(30, 10, (0, 9), &[(15, 0, 1, 1, 'W')]));
        w.place_building(1, BuildingKind::Keep, 20, 2).unwrap();
        let before = w.stocks[&1].amount(Good::Wood);
        let b = w.place_building(1, BuildingKind::Woodcutter, 2, 2).unwrap();
        let wood_after_build = w.stocks[&1].amount(Good::Wood);
        assert_eq!(wood_after_build, before - 20);
        assert!(!w.map.nav.is_open(15, 0), "Wald blockiert");
        // Ein Waldfeld gibt 20 Holz. Der Arbeiter läuft je Stück hin (≈10 Felder), arbeitet 6 s
        // und trägt es zurück; Träger bringen es zum Bergfried.
        run(&mut w, 20.0 * 26.0 + 80.0);
        assert_eq!(w.stocks[&1].amount(Good::Wood), wood_after_build + 20);
        assert_eq!(w.map.resources[15], ResourceKind::None);
        assert!(w.map.nav.is_open(15, 0), "abgeholzt: wieder begehbar");
        let events = w.take_events();
        assert!(events.iter().any(|e| matches!(e, ServerMsg::ResourceChanged { x: 15, y: 0, .. })));
        run(&mut w, 60.0);
        assert_eq!(w.buildings.iter().find(|x| x.id == b.id).unwrap().status, BuildingStatus::NoResource);
    }

    #[test]
    fn werkstatt_braucht_eingaenge_und_rezept_einstellbar() {
        let mut w = world(&map(20, 10, (0, 9), &[]));
        w.place_building(1, BuildingKind::Keep, 10, 0).unwrap();
        let b = w.place_building(1, BuildingKind::Bowyer, 2, 0).unwrap();
        assert_eq!(b.settings.recipe.as_deref(), Some("make_longbow"));
        // Armbrust erst ab Stufe 2.
        let mut s = b.settings.clone();
        s.recipe = Some("make_crossbow".into());
        assert_eq!(w.configure(1, b.id, s.clone()).unwrap_err().key, "error.recipe_locked");
        run(&mut w, 50.0);
        assert!(w.stocks[&1].amount(Good::Longbow) >= 1);
        // Ohne Holz bleibt er stehen, sobald sein eigener Vorrat verbraucht ist.
        let wood = w.stocks[&1].amount(Good::Wood);
        w.stocks.get_mut(&1).unwrap().take_all(&[GoodAmount { good: Good::Wood, amount: wood }]);
        run(&mut w, 60.0);
        let i = w.buildings.iter().position(|x| x.id == b.id).unwrap();
        assert_eq!(w.buildings[i].status, BuildingStatus::MissingInput);
        s.recipe = Some("make_longbow".into());
        s.paused = true;
        w.configure(1, b.id, s).unwrap();
        run(&mut w, 0.1);
        assert_eq!(w.buildings[i].status, BuildingStatus::Paused);
    }

    #[test]
    fn steinbruch_nur_auf_stein_bauernhof_auf_acker() {
        // Stein 3x3 oben rechts, Acker 7x3 unten links (zu wenig für einen 7x7-Hof).
        let mut w = world(&map(20, 20, (19, 19), &[(14, 0, 3, 3, 'S'), (0, 13, 7, 3, 'A')]));
        assert_eq!(w.place_building(1, BuildingKind::Quarry, 0, 0).unwrap_err().key, "error.needs_resource");
        w.place_building(1, BuildingKind::Quarry, 14, 0).unwrap();
        // Mine braucht Eisen, das es hier nicht gibt.
        assert_eq!(w.place_building(1, BuildingKind::Mine, 4, 0).unwrap_err().key, "error.needs_resource");
        // Bauernhof 7x7 auf 21 von 49 Ackerfeldern: zu wenig. Schweine gehen überall.
        assert_eq!(w.place_building(1, BuildingKind::Farm, 0, 9).unwrap_err().key, "error.needs_resource");
        w.place_building(1, BuildingKind::Farm, 0, 12).unwrap_err();
        w.place_building(1, BuildingKind::PigFarm, 8, 8).unwrap();
    }

    #[test]
    fn obstplantage_wird_zu_wald() {
        let mut w = world(&map(14, 10, (13, 9), &[]));
        let b = w.place_building(1, BuildingKind::Orchard, 0, 0).unwrap();
        run(&mut w, 26.0);
        assert_eq!(w.inventories[&b.id].amount(Good::Fruit), 2, "kein Lager: Obst bleibt im Betrieb");
        assert_eq!(w.release_trees(1, b.id + 99).unwrap_err().key, "error.no_building");
        w.release_trees(1, b.id).unwrap();
        assert!(w.buildings.is_empty());
        assert_eq!(w.map.resources[0], ResourceKind::Forest);
        assert!(!w.map.nav.is_open(1, 1));
        // Darauf kann jetzt ein Holzfäller daneben arbeiten.
        w.place_building(1, BuildingKind::Woodcutter, 7, 0).unwrap();
        run(&mut w, 7.0);
        assert_eq!(w.buildings[0].status, BuildingStatus::Working);
    }
}
