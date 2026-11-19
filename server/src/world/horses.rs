//! Pferde und Reiter.
//!
//! Die Pferdezucht züchtet Pferde wie ein normaler Betrieb (Rezept, Arbeiter,
//! Getreide von den Trägern). Die Pferde bleiben in der Zucht: kein Lager nimmt
//! sie, und in eine Zucht passen höchstens `horse_capacity` Stück.
//!
//! Einheiten mit `can_ride` laufen mit `Mount` zur nächsten Zucht mit freiem Pferd
//! und steigen dort auf. Zu Pferd laufen sie mindestens `army.horse_speed`.
//! Treffer gehen erst aufs Pferd (`army.horse_hp`). Fällt es, stürzt der Reiter:
//! er nimmt `army.fall_damage` Schaden und liegt `army.fall_seconds` am Boden,
//! ohne zu laufen oder zu kämpfen. Beim Absteigen läuft das Pferd zurück in die
//! nächste eigene Zucht. Ritter (`mounted`) kommen schon beritten aus der Kaserne.

use protocol::{BuildingState, Good, GoodAmount, Text};

use super::{Refusal, World};

/// So nah (Felder vom Gebäuderand) muss ein Reiter an der Zucht sein, um aufzusteigen.
const MOUNT_REACH: f32 = 2.5;

fn dist_to(b: &BuildingState, p: (f32, f32)) -> f32 {
    let dx = (b.x as f32 - p.0).max(p.0 - (b.x + b.w) as f32).max(0.0);
    let dy = (b.y as f32 - p.1).max(p.1 - (b.y + b.h) as f32).max(0.0);
    dx.hypot(dy)
}

const ONE_HORSE: [GoodAmount; 1] = [GoodAmount { good: Good::Horse, amount: 1 }];

impl World {
    /// Ist das eine Pferdezucht (hat Platz für Pferde)?
    fn is_horse_farm(&self, b: &BuildingState) -> bool {
        self.economy.building(b.kind).is_some_and(|d| d.horse_capacity > 0)
    }

    /// Pferde in dieser Zucht, die noch niemandem versprochen sind.
    fn free_horses(&self, farm: u32) -> u32 {
        let have = self.inventories.get(&farm).map_or(0, |i| i.amount(Good::Horse));
        let promised = self.units.iter().filter(|u| u.mount_at == Some(farm)).count() as u32;
        have.saturating_sub(promised)
    }

    /// Alle freien Pferde eines Spielers.
    pub(crate) fn horses_available(&self, owner: u32) -> u32 {
        self.buildings.iter().filter(|b| b.owner == owner && self.is_horse_farm(b)).map(|b| self.free_horses(b.id)).sum()
    }

    /// Nächste eigene Zucht mit einem freien Pferd.
    fn nearest_free_horse(&self, owner: u32, p: (f32, f32)) -> Option<u32> {
        self.buildings
            .iter()
            .filter(|b| b.owner == owner && self.is_horse_farm(b) && self.free_horses(b.id) > 0)
            .min_by(|a, b| dist_to(a, p).total_cmp(&dist_to(b, p)))
            .map(|b| b.id)
    }

    /// Ein freies Pferd aus der nächsten Zucht nehmen (Ausbildung eines Ritters).
    pub(super) fn take_horse(&mut self, owner: u32, near: (f32, f32)) -> bool {
        let Some(farm) = self.nearest_free_horse(owner, near) else { return false };
        let taken = self.inventories.get_mut(&farm).is_some_and(|i| i.take_all(&ONE_HORSE));
        if taken {
            self.dirty_buildings.insert(farm);
        }
        taken
    }

    /// Pferd zurück in die nächste eigene Zucht, bevorzugt eine mit Platz. Ohne
    /// Zucht läuft es davon.
    pub(super) fn return_horse(&mut self, owner: u32, near: (f32, f32)) {
        let farms: Vec<(f32, u32, bool)> = self
            .buildings
            .iter()
            .filter(|b| b.owner == owner && self.is_horse_farm(b))
            .map(|b| (dist_to(b, near), b.id, self.inventories.get(&b.id).is_some_and(|i| i.room(Good::Horse) > 0)))
            .collect();
        let best = farms
            .iter()
            .filter(|f| f.2)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .or_else(|| farms.iter().min_by(|a, b| a.0.total_cmp(&b.0)));
        let Some(&(_, farm, room)) = best else { return };
        if let Some(inv) = self.inventories.get_mut(&farm) {
            if room {
                inv.add(Good::Horse, 1);
            } else {
                // Alle Zuchten voll: trotzdem zurück, die Zucht pausiert dann, bis wieder Platz ist.
                inv.add_unlimited(Good::Horse, 1);
            }
            self.dirty_buildings.insert(farm);
        }
    }

    /// Kann diese Art reiten?
    fn can_ride(&self, kind: protocol::UnitKind) -> bool {
        self.economy.unit(kind).is_some_and(|d| d.can_ride)
    }

    /// Aufsteigen: wer reiten kann, läuft zur nächsten Zucht mit freiem Pferd.
    pub fn order_mount(&mut self, player: u32, unit_ids: &[u32]) -> Result<u32, Refusal> {
        let mut sent = 0;
        let mut any_rider = false;
        for k in 0..self.units.len() {
            let u = &self.units[k];
            if u.owner != player || !unit_ids.contains(&u.id) || !self.can_ride(u.kind) || u.mounted || u.down > 0.0 {
                continue;
            }
            any_rider = true;
            let (id, pos) = (u.id, u.pos);
            let Some(farm) = self.nearest_free_horse(player, pos) else { break };
            let Some(b) = self.buildings.iter().find(|b| b.id == farm).cloned() else { continue };
            if dist_to(&b, pos) > MOUNT_REACH {
                let center = (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0);
                let door = self.door(&b, pos).unwrap_or_else(|| self.foot_of(center, pos));
                if !self.order_move(player, &[id], door) {
                    continue;
                }
            }
            let u = &mut self.units[k];
            u.attack_order = None;
            u.mount_at = Some(farm);
            sent += 1;
        }
        if sent == 0 {
            return Err(if any_rider { Text::new("error.no_horse") } else { Text::new("error.cannot_ride") });
        }
        Ok(sent)
    }

    /// Absteigen: das Pferd läuft zurück in die Zucht.
    pub fn order_dismount(&mut self, player: u32, unit_ids: &[u32]) -> Result<u32, Refusal> {
        let mut back = Vec::new();
        for u in self.units.iter_mut().filter(|u| u.owner == player && unit_ids.contains(&u.id)) {
            u.mount_at = None;
            if u.mounted {
                u.mounted = false;
                u.horse_damage = 0;
                back.push(u.pos);
            }
        }
        if back.is_empty() {
            return Err(Text::new("error.not_mounted"));
        }
        for p in &back {
            self.return_horse(player, *p);
        }
        Ok(back.len() as u32)
    }

    /// Am Boden liegen, an der Zucht aufsteigen.
    pub(super) fn step_horses(&mut self, dt: f32) {
        for u in self.units.iter_mut().filter(|u| u.down > 0.0) {
            u.down = (u.down - dt).max(0.0);
        }
        let waiting: Vec<usize> = (0..self.units.len()).filter(|&k| self.units[k].mount_at.is_some() && self.units[k].path.is_empty()).collect();
        for k in waiting {
            let u = &self.units[k];
            let Some(farm) = u.mount_at else { continue };
            let near = self.buildings.iter().find(|b| b.id == farm).is_some_and(|b| dist_to(b, u.pos) <= MOUNT_REACH);
            let mut mounted = false;
            if near {
                if let Some(inv) = self.inventories.get_mut(&farm) {
                    mounted = inv.take_all(&ONE_HORSE);
                }
            }
            let u = &mut self.units[k];
            u.mount_at = None;
            if mounted {
                u.mounted = true;
                u.horse_damage = 0;
                self.dirty_buildings.insert(farm);
            }
        }
    }

    /// Treffer gegen eine Einheit: zu Pferd erst aufs Pferd. Liefert `true`, wenn das
    /// Pferd dabei fällt (der Reiter stürzt, nimmt Schaden und bleibt liegen).
    pub(super) fn hurt_unit(&mut self, k: usize, attack: u32) -> bool {
        let army = &self.economy.def.army;
        let (horse_hp, fall_damage, fall_seconds) = (army.horse_hp.max(1), army.fall_damage, army.fall_seconds);
        let u = &mut self.units[k];
        if !u.mounted {
            u.damage = u.damage.saturating_add(attack);
            return false;
        }
        u.horse_damage = u.horse_damage.saturating_add(attack);
        if u.horse_damage < horse_hp {
            return false;
        }
        u.mounted = false;
        u.horse_damage = 0;
        u.damage = u.damage.saturating_add(fall_damage);
        u.down = fall_seconds.max(0.0);
        u.path.clear();
        u.layers.clear();
        u.target = Some(u.pos);
        u.mount_at = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, GoodAmount, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    fn world() -> World {
        // Spieler 2 weit weg, damit sein König das Bauen nicht verhindert.
        let row = "0".repeat(120);
        let text = format!("spawn 1 1\nspawn 118 118\n{}", format!("{row}\n").repeat(120));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.spawn_player(2);
        w
    }

    fn farm_with_horses(w: &mut World, n: u32) -> u32 {
        w.admin_give(1, &[GoodAmount { good: Good::Boards, amount: 200 }, GoodAmount { good: Good::Wood, amount: 200 }], 0);
        let id = w.place_building(1, BuildingKind::HorseFarm, 10, 10).unwrap().id;
        w.inventories.get_mut(&id).unwrap().add(Good::Horse, n);
        id
    }

    fn steps(w: &mut World, seconds: f32) {
        for _ in 0..(seconds * 20.0) as u32 {
            w.step(0.05);
        }
    }

    fn unit(w: &World, id: u32) -> &Unit {
        w.units.iter().find(|u| u.id == id).unwrap()
    }

    #[test]
    fn zucht_hat_begrenzten_platz_fuer_pferde() {
        let mut w = world();
        let farm = farm_with_horses(&mut w, 100);
        let cap = w.economy.building(BuildingKind::HorseFarm).unwrap().horse_capacity;
        assert!(cap > 0);
        assert_eq!(w.inventories[&farm].amount(Good::Horse), cap);
    }

    #[test]
    fn aufsteigen_absteigen_und_pferd_zurueck() {
        let mut w = world();
        let farm = farm_with_horses(&mut w, 1);
        w.units.push(Unit::new(500, 1, UnitKind::Swordsman, (25.0, 25.0)));
        w.units.push(Unit::new(501, 1, UnitKind::Archer, (25.0, 26.0)));
        // Bogenschützen reiten hier nicht (fest gesetzt, unabhängig vom Balancing).
        w.economy.def.units.iter_mut().find(|u| u.kind == UnitKind::Archer).unwrap().can_ride = false;
        assert_eq!(w.order_mount(1, &[501]).unwrap_err().key, "error.cannot_ride");
        assert_eq!(w.order_mount(1, &[500]).unwrap(), 1);
        // Das eine Pferd ist versprochen.
        w.units.push(Unit::new(502, 1, UnitKind::Spearman, (26.0, 25.0)));
        assert_eq!(w.order_mount(1, &[502]).unwrap_err().key, "error.no_horse");
        steps(&mut w, 30.0);
        assert!(unit(&w, 500).mounted, "an der Zucht aufgestiegen");
        assert_eq!(w.inventories[&farm].amount(Good::Horse), 0);
        // Zu Pferd schneller.
        let before = unit(&w, 500).pos;
        w.order_move(1, &[500], (35.0, 35.0));
        steps(&mut w, 1.0);
        let after = unit(&w, 500).pos;
        let walked = (after.0 - before.0).hypot(after.1 - before.1);
        assert!(walked > w.economy.unit(UnitKind::Swordsman).unwrap().speed * 1.2, "{walked}");
        w.order_dismount(1, &[500]).unwrap();
        assert!(!unit(&w, 500).mounted);
        assert_eq!(w.inventories[&farm].amount(Good::Horse), 1, "Pferd wieder in der Zucht");
    }

    #[test]
    fn faellt_das_pferd_stuerzt_der_reiter_und_kaempft_eine_weile_nicht() {
        let mut w = world();
        let army = w.economy.def.army.clone();
        w.units.push(Unit::new(600, 1, UnitKind::Swordsman, (20.0, 20.0)));
        let k = w.units.len() - 1;
        w.units[k].mounted = true;
        assert!(!w.hurt_unit(k, army.horse_hp - 1));
        assert_eq!(w.units[k].damage, 0, "Treffer gehen erst aufs Pferd");
        assert!(w.hurt_unit(k, 5));
        let u = &w.units[k];
        assert!(!u.mounted);
        assert_eq!(u.damage, army.fall_damage);
        assert!(u.down > 0.0);
        // Ein Feind daneben wird nicht angegriffen, solange er liegt.
        w.units.push(Unit::new(601, 2, UnitKind::Spearman, (21.0, 20.0)));
        w.units.last_mut().unwrap().damage = 0;
        // Der Speerträger schlägt zurück; wir prüfen nur, dass der Gestürzte nicht zuschlägt.
        for _ in 0..((army.fall_seconds - 1.0) * 20.0) as u32 {
            w.step_horses(0.05);
            w.step_combat(0.05);
        }
        assert_eq!(w.units.iter().find(|u| u.id == 601).map_or(0, |u| u.damage), 0, "liegt noch und kämpft nicht");
        let pos = unit(&w, 600).pos;
        assert_eq!(pos, (20.0, 20.0), "läuft nicht");
    }

    #[test]
    fn ritter_braucht_ein_pferd_und_kommt_beritten() {
        let mut w = world();
        let knight = w.economy.unit(UnitKind::Knight).unwrap().clone();
        assert!(knight.mounted && knight.can_ride);
        w.admin_give(1, &[GoodAmount { good: Good::Blocks, amount: 200 }, GoodAmount { good: Good::Iron, amount: 200 }, GoodAmount { good: Good::Stone, amount: 200 }], 0);
        let b = w.place_building(1, BuildingKind::Barracks, 25, 10).unwrap().id;
        for _ in 1..w.economy.building(BuildingKind::Barracks).unwrap().levels.len() {
            w.upgrade(1, b).unwrap();
        }
        w.admin_give(1, &knight.cost, 10_000);
        assert_eq!(w.train(1, b, UnitKind::Knight, 1).unwrap_err().key, "error.no_horse");
        let farm = farm_with_horses(&mut w, 1);
        w.train(1, b, UnitKind::Knight, 1).unwrap();
        assert_eq!(w.inventories[&farm].amount(Good::Horse), 0);
        // Streichen gibt das Pferd zurück.
        w.cancel_training(1, b).unwrap();
        assert_eq!(w.inventories[&farm].amount(Good::Horse), 1);
        w.admin_give(1, &knight.cost, 10_000);
        w.train(1, b, UnitKind::Knight, 1).unwrap();
        steps(&mut w, knight.seconds + 1.0);
        let k = w.units.iter().find(|u| u.kind == UnitKind::Knight).expect("Ritter fertig");
        assert!(k.mounted);
    }

    #[test]
    fn angreifen_im_stehen_laufen_reiten_ist_einstellbar() {
        // Ein Speerträger läuft an einem feindlichen Speerträger vorbei.
        let run = |moving: bool, mounted: bool, setup: &dyn Fn(&mut protocol::UnitDef)| {
            let mut w = world();
            for u in w.economy.def.units.iter_mut().filter(|u| u.kind == UnitKind::Spearman) {
                setup(u);
            }
            w.units.push(Unit::new(700, 1, UnitKind::Spearman, (20.0, 20.0)));
            w.units.push(Unit::new(701, 2, UnitKind::Spearman, (21.0, 20.0)));
            let k = w.units.len() - 2;
            w.units[k].mounted = mounted;
            if moving {
                w.units[k].path = vec![(20.0, 30.0)];
            }
            w.step_combat(0.05);
            w.units.iter().find(|u| u.id == 701).unwrap().damage > 0
        };
        assert!(run(false, false, &|_| {}), "im Stehen: ja");
        assert!(!run(true, false, &|_| {}), "im Laufen: nein (Grundeinstellung)");
        assert!(run(true, false, &|u| u.attack_moving = true), "im Laufen, wenn erlaubt");
        assert!(!run(false, false, &|u| u.attack_standing = false), "im Stehen abgeschaltet");
        assert!(run(true, true, &|_| {}), "zu Pferd auch im Reiten");
        assert!(!run(false, true, &|u| u.attack_mounted = false), "zu Pferd abgeschaltet");
    }

    #[test]
    fn alter_spielstand_ohne_pferde_laedt() {
        let json = r#"{"id":1,"owner":1,"kind":"knight","pos":[1.0,1.0],"path":[],"target":null,"home":null,"job":null,"carrying":null,"cooldown":0.0,"gather":null}"#;
        let u: Unit = serde_json::from_str(json).unwrap();
        assert!(!u.mounted && u.down == 0.0);
    }
}
