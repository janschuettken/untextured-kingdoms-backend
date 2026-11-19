//! Heiler aus der Apotheke: Wer `heal` hat, behandelt alle `army.heal_seconds` den am
//! schwersten Verwundeten in Reichweite (`heal_range`), egal ob eigene oder verbündete
//! Einheit, König und Zivilisten eingeschlossen, nur sich selbst nicht. Heilen geht auch
//! im Laufen und über die Ebenen hinweg (vom Wehrgang hinunter und umgekehrt).

use protocol::{Heal, ServerMsg};

use super::World;

impl World {
    pub(super) fn step_heal(&mut self, dt: f32) {
        let every = self.economy.def.army.heal_seconds.max(0.1);
        // Heiler, die gerade behandeln könnten.
        let mut healers = Vec::new();
        let mut any_wounded = false;
        for (k, u) in self.units.iter_mut().enumerate() {
            let Some(def) = self.economy.unit(u.kind) else { continue };
            if def.heal > 0 && def.heal_range > 0.0 {
                u.heal_cooldown = (u.heal_cooldown - dt).max(0.0);
                if u.heal_cooldown <= 0.0 {
                    healers.push((k, def.heal, def.heal_range));
                }
            }
            any_wounded |= u.damage > 0;
        }
        if healers.is_empty() || !any_wounded {
            return;
        }
        let mut heals = Vec::new();
        for (k, amount, range) in healers {
            let h = &self.units[k];
            let mut best: Option<(u32, f32, usize)> = None;
            for (j, t) in self.units.iter().enumerate() {
                if j == k || t.damage == 0 || !(t.owner == h.owner || self.allied(t.owner, h.owner)) {
                    continue;
                }
                // Wer drinnen arbeitet oder wartet, ist nicht zu erreichen.
                if t.kind.is_civilian() && self.inside(t) {
                    continue;
                }
                let d = (t.pos.0 - h.pos.0).hypot(t.pos.1 - h.pos.1);
                if d > range {
                    continue;
                }
                // Am schwersten verwundet zuerst, bei Gleichstand der nächste.
                if best.is_none_or(|(bd, bdist, _)| t.damage > bd || (t.damage == bd && d < bdist)) {
                    best = Some((t.damage, d, j));
                }
            }
            let Some((_, _, j)) = best else { continue };
            let (from, from_wall) = (self.units[k].pos, self.units[k].on_wall);
            self.units[k].heal_cooldown = every;
            let t = &mut self.units[j];
            t.damage = t.damage.saturating_sub(amount);
            heals.push(Heal { from, to: t.pos, from_wall, to_wall: t.on_wall, unit_id: t.id });
        }
        if !heals.is_empty() {
            self.events.push(ServerMsg::Heals { heals });
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{BuildingKind, Good, UnitKind};

    use crate::{economy::Economy, map::GameMap, world::{Unit, World}};

    fn world() -> World {
        let row = "0".repeat(30);
        let text = format!("spawn 1 1\nspawn 28 28\n{}", format!("{row}\n").repeat(30));
        let eco = Economy::parse(crate::economy::ECONOMY_JSON).unwrap().without_start_building().without_people();
        let mut w = World::new(GameMap::parse(&text).unwrap(), eco);
        w.spawn_player(1);
        w.spawn_player(2);
        w.take_events();
        w
    }

    fn put(w: &mut World, owner: u32, kind: UnitKind, pos: (f32, f32)) -> usize {
        let id = 10_000 + w.units.len() as u32;
        w.units.push(Unit::new(id, owner, kind, pos));
        w.units.len() - 1
    }

    #[test]
    fn heiler_heilt_verwundete_in_reichweite() {
        let mut w = world();
        put(&mut w, 1, UnitKind::Healer, (10.5, 10.5));
        let near = put(&mut w, 1, UnitKind::Spearman, (12.5, 10.5));
        let far = put(&mut w, 1, UnitKind::Spearman, (25.5, 25.5));
        w.units[near].damage = 30;
        w.units[far].damage = 30;
        for _ in 0..400 {
            w.step_heal(0.05);
        }
        assert_eq!(w.units[near].damage, 0, "in Reichweite wieder heil");
        assert_eq!(w.units[far].damage, 30, "zu weit weg");
        assert!(w.take_events().iter().any(|e| matches!(e, protocol::ServerMsg::Heals { .. })));
    }

    #[test]
    fn heiler_heilt_keine_feinde_und_nicht_sich_selbst() {
        let mut w = world();
        let h = put(&mut w, 1, UnitKind::Healer, (10.5, 10.5));
        let enemy = put(&mut w, 2, UnitKind::Spearman, (11.5, 10.5));
        w.units[h].damage = 10;
        w.units[enemy].damage = 10;
        for _ in 0..100 {
            w.step_heal(0.05);
        }
        assert_eq!(w.units[h].damage, 10);
        assert_eq!(w.units[enemy].damage, 10);
    }

    #[test]
    fn apotheke_bildet_heiler_aus() {
        let mut w = world();
        w.eliminate(2, None);
        let b = w.place_building(1, BuildingKind::Apothecary, 10, 10).unwrap().id;
        w.stocks.get_mut(&1).unwrap().add_unlimited(Good::Salve, 4);
        w.stocks.get_mut(&1).unwrap().taler = 100;
        w.train(1, b, UnitKind::Healer, 1).unwrap();
        assert!(w.train(1, b, UnitKind::Spearman, 1).is_err(), "Apotheke bildet keine Soldaten aus");
        for _ in 0..(15.0 / 0.1) as u32 {
            w.step_army(0.1);
        }
        assert!(w.units.iter().any(|u| u.owner == 1 && u.kind == UnitKind::Healer));
    }
}
