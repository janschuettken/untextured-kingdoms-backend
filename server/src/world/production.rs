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
    pub(super) elapsed: Option<f32>,
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

        let idle = self.work.get(&id).is_none_or(|w| w.elapsed.is_none());
        if !gathers && idle && self.target_reached(i) {
            return BuildingStatus::TargetReached;
        }
        // Der Arbeiter trägt gerade selbst Waren (siehe `errand.rs`): der Betrieb steht,
        // außer im Teil des Durchlaufs, der von allein läuft (Wachsen).
        if !gathers && self.gather_status.get(&id) == Some(&BuildingStatus::WorkerAway) {
            let grows_alone = self.work.get(&id).and_then(|w| w.elapsed).unwrap_or(0.0)
                < recipe.seconds * recipe.unattended_percent.min(100) as f32 / 100.0;
            if !grows_alone {
                return BuildingStatus::WorkerAway;
            }
        }
        // Abbaubetriebe: die Arbeiter machen die Arbeit (siehe `gather.rs`).
        if gathers {
            return self.gather_status.get(&id).copied().unwrap_or(BuildingStatus::Working);
        }
        let elapsed = self.work.entry(id).or_default().elapsed;
        let elapsed = match elapsed {
            Some(e) => e,
            None => {
                // Höchstbestand erreicht: nicht neu anfangen.
                if self.target_reached(i) {
                    return BuildingStatus::TargetReached;
                }
                // Starten: Rohstoff in Reichweite und Eingangswaren nötig.
                if gathers && self.gather_tile(i).is_none() {
                    return BuildingStatus::NoResource;
                }
                let Some(inv) = self.inventories.get_mut(&id) else { return BuildingStatus::Idle };
                if !inv.take_all(&recipe.inputs) {
                    return if self.held_back(i, &recipe.inputs) {
                        BuildingStatus::StockReserved
                    } else {
                        BuildingStatus::MissingInput
                    };
                }
                if !recipe.inputs.is_empty() {
                    self.dirty_buildings.insert(id);
                }
                0.0
            }
        };
        let elapsed = elapsed + dt * self.speed(i) * self.building_hunger_factor(id);
        self.work.get_mut(&id).expect("eben angelegt").elapsed = Some(elapsed);
        if elapsed < recipe.seconds {
            return BuildingStatus::Working;
        }

        // Fertig: Ergebnis ins eigene Lager, wenn alles hineinpasst. Abholen
        // ist Sache der Träger.
        let inv = self.inventories.get_mut(&id).expect("Betrieb hat ein Lager");
        let blocked: Vec<_> = recipe.outputs.iter().filter(|o| o.amount > inv.room(o.good)).map(|o| o.good).collect();
        if !blocked.is_empty() {
            return self.full_status(self.buildings[i].owner, blocked);
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

    /// Fehlt ein Eingang nur, weil der Mindestbestand (`keep_in_stock`) ihn zurückhält?
    fn held_back(&self, i: usize, inputs: &[protocol::GoodAmount]) -> bool {
        let b = &self.buildings[i];
        let (Some(inv), Some(stock)) = (self.inventories.get(&b.id), self.stocks.get(&b.owner)) else { return false };
        inputs.iter().filter(|g| inv.amount(g.good) < g.amount).any(|g| {
            let keep = b.settings.keep_in_stock.iter().find(|k| k.good == g.good).map_or(0, |k| k.amount);
            keep > 0 && stock.amount(g.good) > 0 && stock.amount(g.good) <= keep
        })
    }

    /// Liegt von einem Ergebnis des laufenden Rezepts schon der Höchstbestand (`stop_at`) im Vorrat?
    pub(super) fn target_reached(&self, i: usize) -> bool {
        let b = &self.buildings[i];
        if b.settings.stop_at.is_empty() {
            return false;
        }
        let Some(recipe) = b.settings.recipe.as_ref().and_then(|r| self.economy.recipe(r)) else { return false };
        let Some(stock) = self.stocks.get(&b.owner) else { return false };
        recipe.outputs.iter().any(|o| {
            b.settings.stop_at.iter().any(|g| g.good == o.good && g.amount > 0 && stock.amount(o.good) >= g.amount)
        })
    }

    /// Höfe auf fruchtbarem Boden arbeiten schneller (anteilig nach Feldern).
    pub(super) fn speed(&self, i: usize) -> f32 {
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
    /// Sucht ringweise nach außen und hört auf, sobald kein näheres Feld mehr kommen kann,
    /// so bleibt auch eine sehr große Reichweite (Holzfäller: ganze Karte) billig.
    pub(super) fn gather_tile(&self, i: usize) -> Option<usize> {
        let b = &self.buildings[i];
        let def = self.economy.building(b.kind)?;
        let (bx0, by0) = (b.x as i64, b.y as i64);
        let (bx1, by1) = (bx0 + b.w as i64 - 1, by0 + b.h as i64 - 1);
        let (mw, mh) = (self.map.width as i64, self.map.height as i64);
        let max_r = (def.at(b.level).gather_radius as i64).min(mw.max(mh));
        // Abstand eines Feldes zum Gebäuderand.
        let dist = |tx: i64, ty: i64| {
            let dx = (bx0 - tx).max(tx - bx1).max(0) as f32;
            let dy = (by0 - ty).max(ty - by1).max(0) as f32;
            dx * dx + dy * dy
        };
        let mut best: Option<(f32, usize)> = None;
        let check = |tx: i64, ty: i64, best: &mut Option<(f32, usize)>| {
            if tx < 0 || ty < 0 || tx >= mw || ty >= mh {
                return;
            }
            let t = (ty * mw + tx) as usize;
            if !def.gathers.contains(&self.map.resources[t]) || self.resource_left[t] == 0 {
                return;
            }
            let d = dist(tx, ty);
            if best.is_none_or(|(bd, _)| d < bd) {
                *best = Some((d, t));
            }
        };
        for r in 0..=max_r {
            // Felder in Ring r liegen mindestens r vom Rand weg.
            if best.is_some_and(|(bd, _)| (r * r) as f32 > bd) {
                break;
            }
            let (x0, y0, x1, y1) = (bx0 - r, by0 - r, bx1 + r, by1 + r);
            if r == 0 {
                for ty in y0..=y1 {
                    for tx in x0..=x1 {
                        check(tx, ty, &mut best);
                    }
                }
                continue;
            }
            if x0 < 0 && y0 < 0 && x1 >= mw && y1 >= mh {
                break; // ganze Karte abgesucht
            }
            for tx in x0..=x1 {
                check(tx, y0, &mut best);
                check(tx, y1, &mut best);
            }
            for ty in y0 + 1..y1 {
                check(x0, ty, &mut best);
                check(x1, ty, &mut best);
            }
        }
        best.map(|(_, t)| t)
    }

    /// Wie `gather_tile`, aber nach Laufzeit statt Luftlinie: von der Tür des Betriebs aus
    /// das Rohstoff-Feld im Umkreis, neben das man am schnellsten kommt (Wege, Hänge und
    /// Umwege um Wald, Wasser und Mauern zählen). Liefert das Feld und den Stehplatz daneben.
    /// Kommt man an keines heran, wie bisher das nächste nach Luftlinie (ohne Stehplatz).
    pub(super) fn fastest_gather_tile(&self, i: usize) -> Option<(usize, Option<(f32, f32)>)> {
        use std::cmp::Reverse;
        use std::collections::{BinaryHeap, HashMap};
        let b = &self.buildings[i];
        let def = self.economy.building(b.kind)?;
        let fallback = || self.gather_tile(i).map(|t| (t, None));
        let r = def.at(b.level).gather_radius as i32;
        let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
        let Some(door) = self.door(b, center) else { return fallback() };
        if r == 0 {
            return fallback();
        }
        let nav = &self.map.nav;
        let (mw, mh) = (self.map.width as i32, self.map.height as i32);
        let (bx0, by0, bx1, by1) = (b.x as i32, b.y as i32, (b.x + b.w) as i32 - 1, (b.y + b.h) as i32 - 1);
        // Im Umkreis: höchstens `r` Felder vom Gebäuderand (wie beim Suchen in Ringen).
        let in_radius = |x: i32, y: i32| (bx0 - x).max(x - bx1).max(0) <= r && (by0 - y).max(y - by1).max(0) <= r;
        let gatherable = |x: i32, y: i32| {
            if x < 0 || y < 0 || x >= mw || y >= mh || !in_radius(x, y) {
                return false;
            }
            let t = (y * mw + x) as usize;
            def.gathers.contains(&self.map.resources[t]) && self.resource_left[t] > 0
        };
        let start = (door.0.floor() as i32, door.1.floor() as i32);
        let mut best: HashMap<(i32, i32), u32> = HashMap::new();
        let mut heap = BinaryHeap::new();
        best.insert(start, 0);
        heap.push(Reverse((0u32, start)));
        while let Some(Reverse((cost, (x, y)))) = heap.pop() {
            if best.get(&(x, y)).is_some_and(|&c| c < cost) {
                continue;
            }
            // Neben einem Rohstoff-Feld: das ist das schnellste.
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if (dx, dy) != (0, 0) && gatherable(x + dx, y + dy) {
                        let t = ((y + dy) * mw + x + dx) as usize;
                        return Some((t, Some((x as f32 + 0.5, y as f32 + 0.5))));
                    }
                }
            }
            if best.len() > 20_000 {
                break;
            }
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    // Etwas über den Umkreis hinaus, damit man auch um Hindernisse herum findet.
                    if (dx, dy) == (0, 0) || !nav.can_step(x, y, dx, dy) || (bx0 - nx).max(nx - bx1).max(0) > r + 8 || (by0 - ny).max(ny - by1).max(0) > r + 8 {
                        continue;
                    }
                    let step = if dx != 0 && dy != 0 { 1.414 } else { 1.0 };
                    let speed = nav.speed_at((nx as f32 + 0.5, ny as f32 + 0.5)).max(0.05);
                    let c = cost + (step / speed * 100.0) as u32;
                    if best.get(&(nx, ny)).is_none_or(|&old| c < old) {
                        best.insert((nx, ny), c);
                        heap.push(Reverse((c, (nx, ny))));
                    }
                }
            }
        }
        fallback()
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
    /// Grenzen einer Ware für neue Betriebe merken (Ressourcen-Fenster). `None` = unverändert, 0 = aus.
    pub fn set_good_limit(&mut self, player: u32, good: protocol::Good, stop_at: Option<u32>, keep_in_stock: Option<u32>) -> Result<(), Refusal> {
        if !self.economy.def.goods.contains(&good) {
            return Err(Text::new("error.unknown_good"));
        }
        let list = self.good_limits.entry(player).or_default();
        let mut l = list.iter().find(|l| l.good == good).copied().unwrap_or(protocol::GoodLimit { good, stop_at: 0, keep_in_stock: 0 });
        l.stop_at = stop_at.unwrap_or(l.stop_at);
        l.keep_in_stock = keep_in_stock.unwrap_or(l.keep_in_stock);
        list.retain(|x| x.good != good);
        if l.stop_at > 0 || l.keep_in_stock > 0 {
            list.push(l);
            list.sort_by_key(|x| x.good);
        }
        self.dirty_stocks.insert(player);
        Ok(())
    }

    /// Startgrenzen für einen neuen Betrieb: (Mindestbestand je Eingang, Höchstbestand je Ergebnis)
    /// aus den gemerkten Grenzen, für alle Rezepte bis zur Stufe `level`.
    pub(super) fn default_limits(&self, player: u32, def: &protocol::BuildingDef, level: u32) -> (Vec<protocol::GoodAmount>, Vec<protocol::GoodAmount>) {
        let Some(limits) = self.good_limits.get(&player) else { return (Vec::new(), Vec::new()) };
        let recipes: Vec<_> = def.levels.iter().take(level.max(1) as usize).flat_map(|l| &l.recipes).filter_map(|r| self.economy.recipe(r)).collect();
        let (mut keep, mut stop) = (Vec::new(), Vec::new());
        for l in limits {
            if l.keep_in_stock > 0 && recipes.iter().any(|r| r.inputs.iter().any(|g| g.good == l.good)) {
                keep.push(protocol::GoodAmount { good: l.good, amount: l.keep_in_stock });
            }
            if l.stop_at > 0 && recipes.iter().any(|r| r.outputs.iter().any(|g| g.good == l.good)) {
                stop.push(protocol::GoodAmount { good: l.good, amount: l.stop_at });
            }
        }
        (keep, stop)
    }

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
        if settings
            .stores
            .iter()
            .flatten()
            .chain(settings.keep_in_stock.iter().chain(&settings.stop_at).map(|g| &g.good))
            .any(|g| !goods.contains(g)) {
            return Err(Text::new("error.unknown_good"));
        }
        // Lieferungen nimmt jeder Betrieb an (der Schalter ist vorerst ausgeblendet).
        let settings = BuildingSettings { accept_deliveries: true, ..settings };
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
        if b.mature_in > 0.0 {
            return Err(Text::new("error.trees_not_grown"));
        }
        self.remove_building(id);
        let tiles: Vec<usize> = self.footprint(&b).collect();
        for t in tiles {
            self.set_resource(t, r);
        }
        Ok(())
    }

    /// Obstplantagen: die Bäume wachsen heran, bis sie als Wald freigegeben werden dürfen.
    pub(super) fn step_orchards(&mut self, dt: f32) {
        for b in &mut self.buildings {
            if b.mature_in > 0.0 {
                b.mature_in = (b.mature_in - dt).max(0.0);
                if b.mature_in == 0.0 {
                    self.dirty_buildings.insert(b.id);
                }
            }
        }
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
        let mut w = World::new(GameMap::parse(text).unwrap(), Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people());
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
    fn neue_betriebe_uebernehmen_die_grenzen_je_ware() {
        let mut w = world(&map(40, 20, (0, 19), &[]));
        w.admin_give(1, &[GoodAmount { good: Good::Wood, amount: 200 }, GoodAmount { good: Good::Boards, amount: 200 }], 0);
        w.set_good_limit(1, Good::Boards, Some(50), None).unwrap();
        w.set_good_limit(1, Good::Wood, None, Some(20)).unwrap();
        assert_eq!(w.stock_state(1).limits.len(), 2);
        let saw = w.place_building(1, BuildingKind::Sawmill, 2, 2).unwrap();
        assert_eq!(saw.settings.stop_at, vec![GoodAmount { good: Good::Boards, amount: 50 }]);
        assert_eq!(saw.settings.keep_in_stock, vec![GoodAmount { good: Good::Wood, amount: 20 }]);
        // Ausschalten: neue Betriebe ohne Grenze.
        w.set_good_limit(1, Good::Boards, Some(0), None).unwrap();
        let saw = w.place_building(1, BuildingKind::Sawmill, 10, 2).unwrap();
        assert!(saw.settings.stop_at.is_empty());
        assert_eq!(saw.settings.keep_in_stock.len(), 1);
    }

    #[test]
    fn holzfaeller_findet_auch_weit_entfernten_wald() {
        let mut w = world(&map(80, 10, (0, 9), &[(70, 1, 1, 1, 'W'), (75, 8, 1, 1, 'W')]));
        w.place_building(1, BuildingKind::Woodcutter, 2, 2).unwrap();
        let bi = w.buildings.len() - 1;
        assert_eq!(w.gather_tile(bi), Some(70 + 80));
    }

    #[test]
    fn holzfaeller_nimmt_den_baum_mit_dem_schnellsten_weg() {
        // Gleich unter dem Betrieb ein Baum, aber hinter einem Felsriegel (langer Umweg).
        // Weiter rechts einer, zu dem man direkt hinläuft.
        let rects = [(0, 15, 34, 1, 'S'), (11, 17, 1, 1, 'W'), (24, 11, 1, 1, 'W')];
        let mut w = world(&map(40, 30, (0, 0), &rects));
        w.place_building(1, BuildingKind::Woodcutter, 10, 10).unwrap();
        let bi = w.buildings.len() - 1;
        assert_eq!(w.gather_tile(bi), Some(17 * 40 + 11), "Luftlinie: der hinter dem Fels");
        let (tile, stand) = w.fastest_gather_tile(bi).unwrap();
        assert_eq!(tile, 11 * 40 + 24, "schnellster Weg: der rechts");
        let (sx, sy) = stand.unwrap();
        assert!((sx - 24.5).abs() <= 1.0 && (sy - 11.5).abs() <= 1.0);
    }

    #[test]
    fn holzfaeller_holzt_wald_ab() {
        let mut w = world(&map(30, 10, (0, 9), &[(15, 0, 1, 1, 'W')]));
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 20, 2).unwrap();
        w.place_building(1, BuildingKind::CartShed, 8, 5).unwrap();
        let before = w.stocks[&1].amount(Good::Wood);
        let b = w.place_building(1, BuildingKind::Woodcutter, 2, 2).unwrap();
        w.test_granary(1); // sonst verdirbt das Essen
        let wood_after_build = w.stocks[&1].amount(Good::Wood);
        let cost = w.economy.building(BuildingKind::Woodcutter).unwrap().levels[0].cost.iter().find(|c| c.good == Good::Wood).map_or(0, |c| c.amount);
        assert_eq!(wood_after_build, before - cost);
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
    fn fortschritt_laeuft_und_wird_selten_geschickt() {
        let mut w = world(&map(20, 10, (0, 9), &[]));
        w.place_building(1, BuildingKind::Keep, 10, 0).unwrap();
        let b = w.place_building(1, BuildingKind::Bowyer, 2, 0).unwrap();
        let mut seen = 0.0f32;
        let mut sent = 0;
        for _ in 0..(90.0 / 0.05) as usize {
            w.step(0.05);
            let x = w.buildings.iter().find(|x| x.id == b.id).unwrap();
            // Geschickt wird Stand + Tempo; der Client rechnet weiter.
            seen = seen.max(x.progress_rate);
            assert!((0.0..=1.0).contains(&x.progress));
            sent += w.take_events().iter().filter(|e| matches!(e, ServerMsg::BuildingChanged { building } if building.id == b.id)).count();
        }
        assert!(seen > 0.0, "Tempo {seen}");
        assert!(sent < 60, "zu oft geschickt: {sent}");
    }

    #[test]
    fn lager_im_betrieb_nach_eingang_und_ergebnis() {
        let mut w = world(&map(20, 10, (0, 9), &[]));
        let b = w.place_building(1, BuildingKind::Bowyer, 2, 0).unwrap();
        let std = w.economy.def.transport.building_capacity;
        assert_eq!(w.inventories[&b.id].capacity_of(Good::Wood), std);
        assert_eq!(w.inventories[&b.id].capacity_of(Good::Longbow), std);
        let mut eco = w.economy.clone();
        let def = eco.def.buildings.iter_mut().find(|d| d.kind == BuildingKind::Bowyer).unwrap();
        def.levels[0].storage_inputs = 30;
        def.levels[0].storage_outputs = 5;
        w.set_economy(eco);
        assert_eq!(w.inventories[&b.id].capacity_of(Good::Wood), 30, "Eingang");
        assert_eq!(w.inventories[&b.id].capacity_of(Good::Longbow), 5, "Ergebnis");
    }

    #[test]
    fn werkstatt_braucht_eingaenge_und_rezept_einstellbar() {
        let mut w = world(&map(20, 10, (0, 9), &[]));
        w.worker_carries_default = false; // hier tragen die Träger
        w.place_building(1, BuildingKind::Keep, 10, 0).unwrap();
        let b = w.place_building(1, BuildingKind::Bowyer, 2, 0).unwrap();
        w.place_building(1, BuildingKind::CartShed, 4, 5).unwrap();
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 50 }, GoodAmount { good: Good::Stone, amount: 50 }], 0);
        w.place_building(1, BuildingKind::Armory, 16, 6).unwrap(); // Platz für Bögen
        w.place_building(1, BuildingKind::Granary, 16, 1).unwrap(); // sonst verdirbt das Essen
        assert_eq!(b.settings.recipe.as_deref(), Some("make_longbow"));
        // Armbrust erst ab Stufe 2.
        let mut s = b.settings.clone();
        s.recipe = Some("make_crossbow".into());
        assert_eq!(w.configure(1, b.id, s.clone()).unwrap_err().key, "error.recipe_locked");
        // Zeit für Anliefern, einen Bogen (Dauer aus dem Balancing) und Abtragen.
        let bow = w.economy.recipe("make_longbow").unwrap().seconds;
        run(&mut w, bow + 40.0);
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
        // 21 von 49 Ackerfeldern (43 %) sollen zu wenig sein, egal, was das Balancing verlangt.
        let mut eco = w.economy.clone();
        eco.def.buildings.iter_mut().find(|d| d.kind == BuildingKind::Farm).unwrap().placement.as_mut().unwrap().min_share_percent = 50;
        w.set_economy(eco);
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
        let fruit = w.economy.recipe("grow_fruit").unwrap().outputs[0].amount;
        assert_eq!(w.inventories[&b.id].amount(Good::Fruit), fruit, "kein Lager: Obst bleibt im Betrieb");
        assert_eq!(w.release_trees(1, b.id + 99).unwrap_err().key, "error.no_building");
        // Die Bäume müssen erst heranwachsen.
        let grow = w.economy.building(BuildingKind::Orchard).unwrap().release_after_seconds;
        if grow > 26.0 {
            assert_eq!(w.release_trees(1, b.id).unwrap_err().key, "error.trees_not_grown");
        }
        let id = b.id;
        w.buildings.iter_mut().find(|b| b.id == id).unwrap().mature_in = 0.1;
        run(&mut w, 0.2);
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
